//! The two-phase kill through a real cell with a real wasm guest,
//! `guests/misbehaver`: `/cooperative` awaits between chunks of work and
//! unwinds on the cancellation it sees at its next slice, `/spin` never awaits
//! and is killed, and `/gc_pin` pins the unsafe-to-trap bit and is
//! killed anyway.

use isoplane_harness::logging::GuestLogs;
use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::{AppManifest, Bindings};
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};

use isoplane_harness::fakes::*;
use isoplane_host::activator::EngineActivator;
use isoplane_host::cell::{Cell, CellConfig, DispatchResult, Services, Target};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::{ModuleId, Resolver};
use isoplane_test_support::wait_for;

/// Large enough that the grace window takes its 10% branch rather than the
/// floor: B = 40 ms, G = 4 ms.
const B_MS: u32 = 40;

struct NoDns;
#[async_trait::async_trait]
impl Resolver for NoDns {
    async fn resolve(
        &self,
        _h: &str,
        _p: u16,
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        Ok(vec![])
    }
}

fn misbehaver_wasm() -> Vec<u8> {
    isoplane_fixtures::rust("misbehaver")
}

fn services() -> Arc<Services> {
    Arc::new(Services {
        egress: Arc::new(HostEgressPolicy::new(
            Box::new(NoDns),
            isoplane_host::egress::ConnectionLimits::uniform(1024).expect("connection limits"),
        )),
        fetcher: Arc::new(FakeFetcher::default()),
    })
}

fn manifest(name: &str) -> AppManifest {
    AppManifest {
        guest_memory_bytes: None,
        resource_limits: Default::default(),
        validation_limits: Default::default(),
        name: name.into(),
        bindings: Bindings::default(),
        init_visible_vars: vec![],
    }
}

fn rig() -> (Arc<Cell>, ModuleId) {
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            max_instances: 64,
            max_memory_bytes: 32 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .expect("engine"),
    );
    let module = ModuleId::new("misbehaver", "v1");
    let act = EngineActivator::load(engine, module.clone(), &misbehaver_wasm())
        .expect("misbehaver.wasm must compile: it is the subject of these tests");
    cell_with(Arc::new(act))
}

fn cell_with(act: Arc<dyn isoplane_host::Activator>) -> (Arc<Cell>, ModuleId) {
    isoplane_harness::logging::LogCapture::global();
    let module = act.module().clone();
    let cell = Cell::new(CellConfig::for_test("isoplane-kill"), services()).expect("a cell starts");
    cell.register_module("acme", act, manifest("misbehaver"))
        .expect("register");
    (cell, module)
}

/// Whether `instance` has left the cell. The response precedes its release,
/// so this waits for it, within a deadline.
fn evicted(cell: &Cell, instance: isoplane_host::InstanceId) -> bool {
    wait_for(|| cell.slot(instance).is_none(), Duration::from_secs(10))
}

fn get(cell: &Cell, module: &ModuleId, path: &str) -> DispatchResult {
    let mut rec = cell.http_record(
        module,
        HttpRequest {
            method: "GET".into(),
            url: format!("http://app.example{path}"),
            path: path.into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        },
    );
    rec.occupancy_budget_ms = B_MS;
    cell.dispatch_blocking(
        module,
        Target::Handler { route: path.into() },
        rec,
        Duration::from_secs(30),
    )
    .expect("dispatch")
}

/// wasmtime does not poison a store after a budget trap, so the cell has to
/// make sure nothing calls into it again: it evicts the instance.
#[test]
fn conformance_1_a_killed_instance_is_evicted_and_never_called_again() {
    let (cell, module) = rig();

    let killed = get(&cell, &module, "/spin");
    assert_eq!(
        killed.completion,
        Completion::BudgetKill,
        "trap: {:?}",
        killed.trap
    );
    assert_eq!(killed.status, 503, "a kill is a 503");

    assert!(
        evicted(&cell, killed.instance),
        "a killed instance must be evicted"
    );

    let m = cell.metrics();
    assert_eq!(m.budget_kills, 1, "one kill");
    assert_eq!(m.budget_exits, 0, "no exit");

    let ok = get(&cell, &module, "/ok");
    assert_eq!(
        ok.status, 200,
        "a killed instance must not take the module down: {:?}",
        ok.trap
    );
    assert_ne!(
        ok.instance, killed.instance,
        "the killed instance must never be reused"
    );

    cell.shutdown();
}

#[test]
fn a_budget_exit_is_measured_and_its_instance_serves_the_next_dispatch_warm() {
    let (cell, module) = cell_with(Arc::new(FakeActivator::named(
        ModuleId::new("misbehaver", "v1"),
        FakeBehaviour::program([Step::BudgetExit { overage_ns: 123 }]),
    )));
    let out = get(&cell, &module, "/cooperative");
    assert_eq!(out.completion, Completion::BudgetExit);
    assert_eq!(out.status, 503);
    assert_eq!(out.occupancy_ns, u64::from(B_MS) * 1_000_000 + 123);
    assert!(
        cell.slot(out.instance).is_some(),
        "a budget exit keeps its instance"
    );
    let metrics = cell.metrics();
    assert_eq!(metrics.budget_exits, 1);
    assert_eq!(metrics.budget_kills, 0);
    assert_eq!(metrics.overage_ns, 123);
    let meters = cell.meters(&module).expect("tenant meters");
    assert_eq!(meters.occupancy_ns(), out.occupancy_ns);
    assert_eq!(meters.budget_exits(), 1);
    assert_eq!(meters.budget_kills(), 0);
    assert_eq!(meters.overage_ns(), 123);
    assert!(
        wait_for(
            || cell.idle_instances(&module) == Some(1),
            Duration::from_secs(30)
        ),
        "the exited instance must return to the pool"
    );
    let next = get(&cell, &module, "/ok");
    assert_eq!(
        (next.status, next.instance, next.acquired),
        (200, out.instance, "warm"),
        "the next dispatch must run on the exited instance as it was left"
    );
    cell.shutdown();
}

#[test]
fn a_guest_that_awaits_between_chunks_unwinds_on_its_cancellation_and_exits() {
    let (cell, module) = rig();
    let out = get(&cell, &module, "/cooperative");
    assert_eq!(
        (out.completion, out.status),
        (Completion::BudgetExit, 503),
        "trap: {:?}",
        out.trap
    );
    let finally_ran = cell
        .slot(out.instance)
        .expect("a budget exit keeps its instance")
        .ctx
        .logs()
        .iter()
        .any(|log| log.fields.contains("finally ran"));
    assert!(finally_ran, "the guest's Drop must run before it exits");
    let metrics = cell.metrics();
    assert_eq!((metrics.budget_exits, metrics.budget_kills), (1, 0));
    cell.shutdown();
}

/// If `/spin` ever exited, the grace window would be a budget extension for
/// everybody.
#[test]
fn negative_control_an_uncooperative_guest_cannot_exit() {
    let (cell, module) = rig();

    for route in ["/spin", "/gc_pin"] {
        let out = get(&cell, &module, route);
        assert_eq!(
            out.completion,
            Completion::BudgetKill,
            "{route} was let exit; trap: {:?}",
            out.trap
        );
        assert_ne!(out.completion, Completion::BudgetExit, "{route} exited");
        assert!(evicted(&cell, out.instance), "{route} must be evicted");
    }

    let m = cell.metrics();
    assert_eq!(m.budget_kills, 2, "both routes were killed");
    assert_eq!(m.budget_exits, 0, "no exit");
    assert_eq!(m.exit_kill_ratio, 0.0, "the ratio reads unhealthy");

    cell.shutdown();
}

#[test]
fn the_hang_breaker_counts_only_budget_kills() {
    for (step, exits, kills, rejected) in [
        (Step::BudgetExit { overage_ns: 123 }, 6, 0, 0),
        (Step::HardKill, 0, 3, 3),
    ] {
        let (cell, module) = cell_with(Arc::new(FakeActivator::named(
            ModuleId::new("misbehaver", "v1"),
            FakeBehaviour::program(vec![step; 6]),
        )));
        for _ in 0..6 {
            get(&cell, &module, "/budget");
        }
        let metrics = cell.metrics();
        assert_eq!(metrics.budget_exits, exits);
        assert_eq!(metrics.budget_kills, kills);
        assert_eq!(metrics.breaker_rejections, rejected);
        cell.shutdown();
    }
}
