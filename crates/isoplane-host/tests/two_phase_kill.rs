//! The two-phase kill through a real cell with a real wasm guest,
//! `guests/misbehaver`: `/cooperative` awaits between chunks of work and
//! unwinds on the cancellation it sees at its next slice, `/spin` never awaits
//! and is hard-killed, and `/gc_pin` pins the unsafe-to-trap bit and is
//! hard-killed anyway.

use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::{AppManifest, Bindings, Limits};
use isoplane_abi::dispatch::HttpRequest;
use isoplane_engine::{Completion, EngineConfig, IsoplaneEngine};

use isoplane_host::activator::EngineActivator;
use isoplane_host::cell::{Cell, CellConfig, DispatchResult, Services, Target};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::fakes::*;
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
        limits: Limits::default(),
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
    let module = act.module().clone();
    let cell = Cell::new(CellConfig::for_test("isoplane-kill"), services()).expect("a cell starts");
    cell.register_module("acme", act, manifest("misbehaver"))
        .expect("register");
    (cell, module)
}

/// Whether `instance` has left the cell. The response precedes its release,
/// so this waits for it, within a deadline.
fn evicted(cell: &Cell, instance: isoplane_host::InstanceId) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while cell.slot(instance).is_some() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    cell.slot(instance).is_none()
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
    assert_eq!(killed.status, 503, "a hard kill is a 503");

    assert!(
        evicted(&cell, killed.instance),
        "a hard-killed instance must be evicted"
    );

    let m = cell.metrics();
    assert_eq!(m.hard_kills, 1, "one hard kill");
    assert_eq!(m.clean_kills, 0, "no clean kill");

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
fn a_clean_kill_is_recorded_without_evicting_the_instance() {
    let (cell, module) = cell_with(Arc::new(FakeActivator::named(
        ModuleId::new("misbehaver", "v1"),
        FakeBehaviour::program([Step::CleanKill { overage_ns: 123 }]),
    )));
    let out = get(&cell, &module, "/cooperative");
    assert_eq!(out.completion, Completion::CleanKill);
    assert_eq!(out.status, 503);
    assert_eq!(out.occupancy_ns, u64::from(B_MS) * 1_000_000 + 123);
    assert!(
        cell.slot(out.instance).is_some(),
        "a clean kill keeps its instance"
    );
    let metrics = cell.metrics();
    assert_eq!(metrics.clean_kills, 1);
    assert_eq!(metrics.hard_kills, 0);
    assert_eq!(metrics.overage_ns, 123);
    let meters = cell.meters(&module).expect("tenant meters");
    assert_eq!(meters.occupancy_ns(), out.occupancy_ns);
    assert_eq!(meters.clean_kills(), 1);
    assert_eq!(meters.hard_kills(), 0);
    assert_eq!(meters.overage_ns(), 123);
    cell.shutdown();
}

#[test]
fn a_guest_that_awaits_between_chunks_unwinds_on_its_cancellation_and_takes_the_clean_kill() {
    TestLogCapture::global();
    let (cell, module) = rig();
    let out = get(&cell, &module, "/cooperative");
    assert_eq!(
        (out.completion, out.status),
        (Completion::CleanKill, 503),
        "trap: {:?}",
        out.trap
    );
    let finally_ran = cell
        .slot(out.instance)
        .expect("a clean kill keeps its instance")
        .ctx
        .emitted_logs()
        .iter()
        .any(|log| log.fields.contains("finally ran"));
    assert!(finally_ran, "the guest's Drop must run on a clean kill");
    let metrics = cell.metrics();
    assert_eq!((metrics.clean_kills, metrics.hard_kills), (1, 0));
    cell.shutdown();
}

/// If `/spin` ever came back clean, the grace window would be a budget
/// extension for everybody.
#[test]
fn negative_control_an_uncooperative_guest_cannot_reach_the_clean_path() {
    let (cell, module) = rig();

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
        assert!(evicted(&cell, out.instance), "{route} must be evicted");
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
        let (cell, module) = cell_with(Arc::new(FakeActivator::named(
            ModuleId::new("misbehaver", "v1"),
            FakeBehaviour::program([step]),
        )));
        for _ in 0..6 {
            get(&cell, &module, "/budget");
        }
        let metrics = cell.metrics();
        assert_eq!(metrics.clean_kills, clean);
        assert_eq!(metrics.hard_kills, hard);
        assert_eq!(metrics.breaker_rejections, rejected);
        cell.shutdown();
    }
}

#[derive(Clone, Default)]
struct TestLogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

#[derive(Debug, serde::Deserialize)]
struct TestGuestLog {
    fields: String,
}

impl TestLogCapture {
    fn global() -> &'static Self {
        use tracing_subscriber::prelude::*;
        static CAPTURE: std::sync::OnceLock<TestLogCapture> = std::sync::OnceLock::new();
        CAPTURE.get_or_init(|| {
            let capture = Self::default();
            let subscriber = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_writer(capture.clone())
                    .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                        metadata.target().contains("host::bindings")
                    })),
            );
            tracing::subscriber::set_global_default(subscriber)
                .expect("one guest-event subscriber per test process");
            capture
        })
    }

    fn for_instance(&self, instance: impl std::fmt::Display) -> Vec<TestGuestLog> {
        let instance = instance.to_string();
        let bytes = self.0.lock().expect("test writer mutex").clone();
        String::from_utf8(bytes)
            .expect("tracing emits UTF-8")
            .lines()
            .filter_map(|line| {
                let event: serde_json::Value =
                    serde_json::from_str(line).expect("tracing JSON event");
                if event["fields"]["message"] != "guest log"
                    || event["fields"]["instance"] != instance
                {
                    return None;
                }
                Some(
                    serde_json::from_value(event["fields"].clone())
                        .expect("the guest logging boundary emits text"),
                )
            })
            .collect()
    }
}

impl std::io::Write for TestLogCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("test writer mutex")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for TestLogCapture {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

trait EmittedGuestLogs {
    fn emitted_logs(&self) -> Vec<TestGuestLog>;
}

impl EmittedGuestLogs for isoplane_host::bindings::InstanceCtx {
    fn emitted_logs(&self) -> Vec<TestGuestLog> {
        TestLogCapture::global().for_instance(self.instance)
    }
}
