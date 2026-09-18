//! The two-phase kill through a real cell with a real wasm guest,
//! `guests/misbehaver`: `/cooperative` reads `budget_remaining_ns` and is
//! cleanly killed, `/spin` never checks and is hard-killed at the deadline,
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
const G_NS: u64 = 4_000_000;
const HARD_NS: u64 = 44_000_000;

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
        egress: Arc::new(HostEgressPolicy::new(Box::new(NoDns), 1024, 1 << 30)),
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
    let cell = Cell::new(CellConfig::for_test("isoplane-kill"), services()).expect("a cell starts");
    cell.register_module("acme", Arc::new(act), manifest("misbehaver", lifecycle))
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
    rec.cpu_budget_ms = B_MS;
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

/// `/cooperative` is `/spin` plus a read of the remaining budget at its own
/// safepoints, so the two routes are a controlled comparison.
#[test]
fn conformance_2_a_dispatch_that_yields_during_grace_is_clean() {
    let (cell, module) = rig(HandlerLifecycle::Reuse);

    let out = get(&cell, &module, "/cooperative");

    assert_eq!(
        out.completion,
        Completion::CleanKill,
        "trap: {:?}",
        out.trap
    );
    assert_eq!(out.status, 503, "a clean kill is still a 503");
    assert!(
        out.trap
            .as_deref()
            .unwrap_or_default()
            .contains("budget_exceeded"),
        "a clean kill still reports budget_exceeded: {:?}",
        out.trap
    );

    let slot = cell.slot(out.instance);
    if let Some(s) = &slot {
        assert!(!s.is_tombstoned(), "a clean kill must not tombstone");
    }

    let cpu_ms = out.cpu_ns as f64 / 1e6;
    assert!(
        cpu_ms >= B_MS as f64,
        "the dispatch must actually have crossed B: {cpu_ms:.3} ms"
    );
    assert!(
        out.cpu_ns < HARD_NS,
        "a clean kill must land inside the grace window: {cpu_ms:.3} ms against a {:.0} ms hard \
         deadline",
        HARD_NS as f64 / 1e6
    );

    let m = cell.metrics();
    assert_eq!(m.clean_kills, 1, "one clean kill");
    assert_eq!(
        m.hard_kills, 0,
        "a cooperative guest must never take the hard path"
    );
    assert_eq!(m.tombstones, 0, "a clean kill leaves no tombstone");
    assert!(m.overage_ns > 0, "the overage CPU is recorded");
    assert!(
        m.overage_ns < G_NS,
        "the recorded overage exceeded G, which would mean the clean kill landed past B+G: {} ns",
        m.overage_ns
    );

    let meters = cell.meters(&module).expect("tenant meters");
    assert_eq!(
        meters.clean_kills(),
        1,
        "the tenant meter counts the clean kill"
    );
    assert_eq!(meters.hard_kills(), 0, "and no hard kill");
    assert!(
        meters.cpu_ns() >= (B_MS as u64) * 1_000_000,
        "the tenant was recorded {} ns for a dispatch that burned its whole {B_MS} ms budget",
        meters.cpu_ns()
    );
    assert_eq!(
        meters.overage_ns(),
        m.overage_ns,
        "the tenant meter and the cell metrics agree on the overage"
    );

    for _ in 0..3 {
        let ok = get(&cell, &module, "/ok");
        assert_eq!(ok.status, 200, "{:?}", ok.trap);
    }

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

/// Both sides are pinned: early means the grace window was not offered, late
/// means the deadline is soft. The bound is the epoch tick plus scheduling
/// noise.
#[test]
fn conformance_3_a_genuine_spin_tombstones_at_exactly_b_plus_g() {
    let (cell, module) = rig(HandlerLifecycle::Reuse);

    for route in ["/spin", "/gc_pin"] {
        let mut samples = Vec::new();
        for _ in 0..5 {
            let out = get(&cell, &module, route);
            assert_eq!(
                out.completion,
                Completion::BudgetKill,
                "{route}: {:?}",
                out.trap
            );
            assert!(
                out.cpu_ns >= HARD_NS,
                "{route} was killed after {} ns, BEFORE the B+G deadline of {HARD_NS} ns — the \
                 grace window was cut short, which would make the clean path unreachable for a \
                 guest that needs it",
                out.cpu_ns
            );
            samples.push(out.cpu_ns - HARD_NS);
            // The breaker opens after three hard kills.
            cell.clear_breaker(&module);
        }
        samples.sort_unstable();
        let p50 = samples[samples.len() / 2];
        assert!(
            p50 < G_NS / 2,
            "{route}: the hard deadline is soft — p50 overshoot {p50} ns against G = {G_NS} ns"
        );
    }

    cell.shutdown();
}

/// A breaker that counted clean kills would fail the tenants who cooperate
/// best.
#[test]
fn the_hang_breaker_counts_only_hard_kills() {
    let (cell, module) = rig(HandlerLifecycle::Reuse);

    // Well past the default threshold of three.
    for _ in 0..6 {
        let out = get(&cell, &module, "/cooperative");
        assert_eq!(out.completion, Completion::CleanKill, "{:?}", out.trap);
    }
    let m = cell.metrics();
    assert_eq!(
        m.clean_kills, 6,
        "every cooperative dispatch was cleanly killed"
    );
    assert_eq!(
        m.breaker_rejections, 0,
        "clean kills must not trip the hang breaker"
    );
    assert!(m.overage_ns > 0, "…and must still be visible in telemetry");

    // The same count of hard kills does trip it.
    for _ in 0..4 {
        let _ = get(&cell, &module, "/spin");
    }
    let after = cell.metrics();
    assert!(
        after.breaker_rejections > 0,
        "four hard kills on one route must trip the breaker, or this test proves nothing about \
         clean kills"
    );

    assert_eq!(after.clean_kills, 6, "the clean count is unchanged");
    assert!(after.hard_kills >= 3, "the spins were hard-killed");

    cell.shutdown();
}

/// `/cooperative` holds a `Drop` guard that logs on the way out; the log line
/// is the observable.
#[test]
fn a_clean_kill_runs_finally_and_a_hard_kill_does_not() {
    TestLogCapture::global();
    let (cell, module) = rig(HandlerLifecycle::Reuse);

    let clean = get(&cell, &module, "/cooperative");
    assert_eq!(clean.completion, Completion::CleanKill, "{:?}", clean.trap);
    let logs = cell
        .slot(clean.instance)
        .map(|s| s.ctx.emitted_logs())
        .unwrap_or_default();
    assert!(
        logs.iter().any(|l| l.fields.contains("finally ran")),
        "a clean kill must run the guest's `finally`; logs were {:?}",
        logs.iter().map(|l| l.fields.clone()).collect::<Vec<_>>()
    );

    let hard = get(&cell, &module, "/spin");
    assert_eq!(
        hard.completion,
        Completion::BudgetKill,
        "the spin is hard-killed"
    );
    let slot = cell.slot(hard.instance).expect("addressable");
    assert!(slot.is_tombstoned(), "a hard kill tombstones");

    cell.shutdown();
}

/// No threshold on the ratio, which this harness controls entirely; what is
/// asserted is the separation between a cooperative guest and a spinning one.
#[test]
fn the_clean_hard_ratio_separates_a_cooperative_guest_from_a_spinning_one() {
    const N: usize = 12;

    for route in ["/cooperative", "/spin"] {
        // A fresh cell per guest so the counters are that guest's alone.
        let (cell, module) = rig(HandlerLifecycle::Reuse);
        for _ in 0..N {
            let _ = get(&cell, &module, route);
            // The breaker exists precisely to stop paying for this, so it
            // would open after three hard kills and shed the rest. Clearing it
            // keeps the sample size honest; that clean kills do NOT open it is
            // asserted in `the_hang_breaker_counts_only_hard_kills`.
            cell.clear_breaker(&module);
        }

        let m = cell.metrics();
        if route == "/cooperative" {
            assert_eq!(
                m.clean_kills as usize, N,
                "every cooperative request was cleanly killed"
            );
            assert_eq!(m.hard_kills, 0, "a cooperative guest produced a hard kill");
            assert_eq!(m.tombstones, 0, "and must never tombstone");
            assert!(
                m.overage_ns < G_NS * N as u64,
                "average overage {:.3} ms exceeded G = {:.0} ms",
                m.overage_ns as f64 / 1e6 / N as f64,
                G_NS as f64 / 1e6
            );
        } else {
            assert_eq!(
                m.hard_kills as usize, N,
                "every spinning request was hard-killed"
            );
            assert_eq!(
                m.clean_kills, 0,
                "an uncooperative guest reached the clean path"
            );
            assert_eq!(m.tombstones as usize, N, "every hard kill must tombstone");
            assert_eq!(
                m.clean_hard_ratio, 0.0,
                "the SLI must be able to say `unhealthy`, or it is unfalsifiable"
            );
        }
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
