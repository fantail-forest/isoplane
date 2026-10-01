//! The two-phase kill through a real cell with a real wasm guest,
//! `guests/misbehaver`: `/cooperative` reads `budget_remaining_ns` and is
//! cooperates with cancellation, `/spin` never checks and is hard-killed,
//! and `/gc_pin` pins the unsafe-to-trap bit and is hard-killed anyway.

use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};

use isoplane_host::activator::EngineActivator;
use isoplane_host::cell::{Cell, CellConfig, DispatchResult, Services, Target};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::fakes::*;
use isoplane_host::slot::EnterError;
use isoplane_host::{ModuleId, Resolver};

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
        egress: Arc::new(HostEgressPolicy::new(Box::new(NoDns), 1024)),
        fetcher: Arc::new(FakeFetcher::default()),
    })
}

fn manifest(name: &str, lifecycle: HandlerLifecycle) -> AppManifest {
    AppManifest {
        guest_memory_bytes: None,
        resource_limits: Default::default(),
        validation_limits: Default::default(),
        name: name.into(),
        handlers: HandlerConfig { lifecycle },
        bindings: Bindings::default(),
        limits: Limits::default(),
        init_visible_vars: vec![],
    }
}

fn rig(lifecycle: HandlerLifecycle) -> (Arc<Cell>, ModuleId) {
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
    cell_with(lifecycle, Arc::new(act))
}

fn cell_with(
    lifecycle: HandlerLifecycle,
    act: Arc<dyn isoplane_host::Activator>,
) -> (Arc<Cell>, ModuleId) {
    let module = act.module().clone();
    let cell = Cell::new(CellConfig::for_test("isoplane-kill"), services()).expect("a cell starts");
    cell.register_module("acme", act, manifest("misbehaver", lifecycle))
        .expect("register");
    (cell, module)
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

/// wasmtime does not poison a store after a budget trap, so the refusal has
/// to come from the cell.
#[test]
fn conformance_1_kill_then_call_observes_refusal() {
    let (cell, module) = rig(HandlerLifecycle::Reuse);

    let killed = get(&cell, &module, "/spin");
    assert_eq!(
        killed.completion,
        Completion::BudgetKill,
        "trap: {:?}",
        killed.trap
    );
    assert_eq!(killed.status, 503, "a hard kill is a 503");

    let slot = cell
        .slot(killed.instance)
        .expect("the killed slot is still addressable");
    assert!(slot.is_tombstoned(), "a hard kill must leave a tombstone");
    assert_eq!(
        slot.enter().err(),
        Some(EnterError::Tombstoned),
        "a tombstoned instance answered a host call"
    );

    assert!(
        !slot.is_capture_eligible(),
        "deltas are never captured from a tombstone"
    );
    assert!(
        slot.reset().is_err(),
        "a tombstone is discarded, not reset back into the pool"
    );

    let m = cell.metrics();
    assert_eq!(m.hard_kills, 1, "one hard kill");
    assert_eq!(m.clean_kills, 0, "no clean kill");
    assert_eq!(m.tombstones, 1, "one tombstone");

    let ok = get(&cell, &module, "/ok");
    assert_eq!(
        ok.status, 200,
        "a tombstone must not take the module down: {:?}",
        ok.trap
    );
    assert_ne!(
        ok.instance, killed.instance,
        "the tombstoned instance must never be reused"
    );

    cell.shutdown();
}

#[test]
fn a_clean_kill_is_recorded_without_tombstoning_the_instance() {
    let (cell, module) = cell_with(
        HandlerLifecycle::Reuse,
        Arc::new(FakeActivator::named(
            ModuleId::new("misbehaver", "v1"),
            FakeBehaviour::program([Step::CleanKill { overage_ns: 123 }]),
        )),
    );
    let out = get(&cell, &module, "/cooperative");
    assert_eq!(out.completion, Completion::CleanKill);
    assert_eq!(out.status, 503);
    assert_eq!(out.occupancy_ns, u64::from(B_MS) * 1_000_000 + 123);
    if let Some(slot) = cell.slot(out.instance) {
        assert!(!slot.is_tombstoned());
    }
    let metrics = cell.metrics();
    assert_eq!(metrics.clean_kills, 1);
    assert_eq!(metrics.hard_kills, 0);
    assert_eq!(metrics.tombstones, 0);
    assert_eq!(metrics.overage_ns, 123);
    let meters = cell.meters(&module).expect("tenant meters");
    assert_eq!(meters.occupancy_ns(), out.occupancy_ns);
    assert_eq!(meters.clean_kills(), 1);
    assert_eq!(meters.hard_kills(), 0);
    assert_eq!(meters.overage_ns(), 123);
    cell.shutdown();
}

/// If `/spin` ever came back clean, the grace window would be a budget
/// extension for everybody.
#[test]
fn negative_control_an_uncooperative_guest_cannot_reach_the_clean_path() {
    let (cell, module) = rig(HandlerLifecycle::Reuse);

    for route in ["/spin", "/gc_pin"] {
        let out = get(&cell, &module, route);
        assert_eq!(
            out.completion,
            Completion::BudgetKill,
            "{route} was let onto the clean path; trap: {:?}",
            out.trap
        );
        assert_ne!(
            out.completion,
            Completion::CleanKill,
            "{route} reached the clean path"
        );
        let slot = cell.slot(out.instance).expect("addressable");
        assert!(slot.is_tombstoned(), "{route} must tombstone");
    }

    let m = cell.metrics();
    assert_eq!(m.hard_kills, 2, "both routes were hard-killed");
    assert_eq!(m.clean_kills, 0, "no clean kill");
    assert_eq!(m.clean_hard_ratio, 0.0, "the ratio reads unhealthy");

    cell.shutdown();
}

#[test]
fn the_hang_breaker_counts_only_hard_kills() {
    for (step, clean, hard, rejected) in [
        (Step::CleanKill { overage_ns: 123 }, 6, 0, 0),
        (Step::HardKill, 0, 3, 3),
    ] {
        let (cell, module) = cell_with(
            HandlerLifecycle::Reuse,
            Arc::new(FakeActivator::named(
                ModuleId::new("misbehaver", "v1"),
                FakeBehaviour::program([step]),
            )),
        );
        for _ in 0..6 {
            get(&cell, &module, "/budget");
        }
        let metrics = cell.metrics();
        assert_eq!(metrics.clean_kills, clean);
        assert_eq!(metrics.hard_kills, hard);
        assert_eq!(metrics.breaker_rejections, rejected);
        assert_eq!(metrics.tombstones, hard);
        cell.shutdown();
    }
}
