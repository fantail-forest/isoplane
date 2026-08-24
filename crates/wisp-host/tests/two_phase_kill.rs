//! Cooperative and forced budget termination through a real cell and WASM guest.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};
use wisp_abi::dispatch::HttpRequest;
use wisp_engine::{Completion, EngineConfig, WispEngine};

use wisp_host::activator::EngineActivator;
use wisp_host::cell::{Cell, CellConfig, DispatchResult, Services, Target};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::*;
use wisp_host::instances::EnterError;
use wisp_host::{ModuleId, Resolver};

const B_MS: u32 = 40;
const G_NS: u64 = 4_000_000;
const HARD_NS: u64 = 44_000_000;

struct NoDns;
impl Resolver for NoDns {
    fn resolve(&self, _h: &str, _p: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Ok(vec![])
    }
}

fn misbehaver_wasm() -> Vec<u8> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/wisp-host has a grandparent")
        .to_path_buf();
    let p = root.join("guests/out/misbehaver.wasm");
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}.\n\nThese are the §6.4 conformance tests and they are only \
             meaningful against a REAL guest — a grace window demonstrated against a fake proves \
             the fake cooperates, not the platform. Build the guests first:\n    \
             scripts/dev guests/build.sh",
            p.display()
        )
    })
}

fn services() -> Arc<Services> {
    Arc::new(Services {
        egress: Arc::new(HostEgressPolicy::new(Box::new(NoDns), 1024, 1 << 30)),
        fetcher: Arc::new(FakeFetcher::default()),
    })
}

fn manifest(name: &str, lifecycle: HandlerLifecycle) -> AppManifest {
    AppManifest {
        name: name.into(),
        handlers: HandlerConfig { lifecycle },
        bindings: Bindings::default(),
        limits: Limits::default(),
        init_visible_vars: vec![],
    }
}

/// A cell with the real engine and the real misbehaver module loaded.
fn rig(lifecycle: HandlerLifecycle) -> (Arc<Cell>, ModuleId) {
    let engine = Arc::new(
        WispEngine::new(EngineConfig {
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
    let cell = Cell::new(CellConfig::for_test("wisp-kill"), services());
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
    assert_eq!(killed.status, 503);

    // The instance that was killed, addressed directly — the same door F2
    // walked through.
    let slot = cell
        .slot(killed.instance)
        .expect("the killed slot is still addressable");
    assert!(
        slot.is_tombstoned(),
        "§6.4: a hard kill must leave a tombstone"
    );
    assert_eq!(
        slot.enter().err(),
        Some(EnterError::Tombstoned),
        "a tombstoned instance answered a host call — this is exactly Appendix C F2"
    );

    assert!(
        !slot.is_capture_eligible(),
        "§6.4: deltas are never captured from a tombstone"
    );
    assert!(
        slot.reset().is_err(),
        "§6.4: a tombstone is discarded, not reset back into the pool"
    );

    let m = cell.metrics();
    assert_eq!(m.hard_kills, 1);
    assert_eq!(m.clean_kills, 0);
    assert_eq!(m.tombstones, 1);

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
fn conformance_2_a_dispatch_that_yields_during_grace_is_clean() {
    let (cell, module) = rig(HandlerLifecycle::Reuse);

    let out = get(&cell, &module, "/cooperative");

    assert_eq!(
        out.completion,
        Completion::CleanKill,
        "trap: {:?}",
        out.trap
    );
    assert_eq!(out.status, 503);
    assert!(
        out.trap
            .as_deref()
            .unwrap_or_default()
            .contains("budget_exceeded"),
        "a clean kill still reports budget_exceeded (§13): {:?}",
        out.trap
    );

    // NOT a tombstone. This is the whole difference the grace window buys.
    let slot = cell.slot(out.instance);
    if let Some(s) = &slot {
        assert!(!s.is_tombstoned(), "§6.4: a clean kill must not tombstone");
    }

    // The overage is real, recorded, and inside G.
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
    assert_eq!(m.clean_kills, 1);
    assert_eq!(
        m.hard_kills, 0,
        "a cooperative guest must never take the hard path"
    );
    assert_eq!(m.tombstones, 0);
    assert!(m.overage_ns > 0, "§6.4: the overage CPU is recorded");
    assert!(
        m.overage_ns < G_NS,
        "the recorded overage exceeded G, which would mean the clean kill landed past B+G: {} ns",
        m.overage_ns
    );

    let meters = cell.meters(&module).expect("tenant meters");
    assert_eq!(meters.clean_kills(), 1);
    assert_eq!(meters.hard_kills(), 0);
    assert!(
        meters.cpu_ns() >= (B_MS as u64) * 1_000_000,
        "the tenant was recorded {} ns for a dispatch that burned its whole {B_MS} ms budget",
        meters.cpu_ns()
    );
    assert_eq!(meters.overage_ns(), m.overage_ns);

    eprintln!(
        "CLEAN KILL (real wasm guest): B={B_MS} ms, G={:.0} ms; returned after {cpu_ms:.3} ms \
         CPU, {:.3} ms overage ({:.1}% of G), slot {}",
        G_NS as f64 / 1e6,
        m.overage_ns as f64 / 1e6,
        100.0 * m.overage_ns as f64 / G_NS as f64,
        if slot.is_some() {
            "retained"
        } else {
            "recycled"
        }
    );

    for _ in 0..3 {
        let ok = get(&cell, &module, "/ok");
        assert_eq!(ok.status, 200, "{:?}", ok.trap);
    }

    cell.shutdown();
}

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
        assert_ne!(out.completion, Completion::CleanKill);
        let slot = cell.slot(out.instance).expect("addressable");
        assert!(slot.is_tombstoned(), "{route} must tombstone");
    }

    let m = cell.metrics();
    assert_eq!(m.hard_kills, 2);
    assert_eq!(m.clean_kills, 0);
    assert_eq!(m.clean_hard_ratio, 0.0);

    cell.shutdown();
}

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
            // The breaker opens after three hard kills, so close it between
            // samples: this test is about the deadline, and
            // `the_hang_breaker_counts_only_hard_kills` is about the breaker.
            cell.clear_breaker(&module);
        }
        samples.sort_unstable();
        let p50 = samples[samples.len() / 2];
        let max = *samples.last().unwrap();
        eprintln!(
            "HARD KILL (real wasm guest) {route}: B+G = {:.0} ms; overshoot past it \
             p50={:.0} us max={:.0} us",
            HARD_NS as f64 / 1e6,
            p50 as f64 / 1000.0,
            max as f64 / 1000.0
        );
        assert!(
            p50 < G_NS / 2,
            "{route}: the hard deadline is soft — p50 overshoot {p50} ns against G = {G_NS} ns"
        );
    }

    cell.shutdown();
}

#[test]
fn the_hang_breaker_counts_only_hard_kills() {
    let (cell, module) = rig(HandlerLifecycle::Reuse);

    // Well past the default threshold of three.
    for _ in 0..6 {
        let out = get(&cell, &module, "/cooperative");
        assert_eq!(out.completion, Completion::CleanKill, "{:?}", out.trap);
    }
    let m = cell.metrics();
    assert_eq!(m.clean_kills, 6);
    assert_eq!(
        m.breaker_rejections, 0,
        "clean kills must not trip the hang breaker"
    );
    assert!(m.overage_ns > 0, "…and must still be visible in telemetry");

    // The same route count of HARD kills does trip it, so the test is not
    // passing because the breaker is broken.
    for _ in 0..4 {
        let _ = get(&cell, &module, "/spin");
    }
    let after = cell.metrics();
    assert!(
        after.breaker_rejections > 0,
        "four hard kills on one route must trip the breaker, or this test proves nothing about \
         clean kills"
    );

    assert_eq!(after.clean_kills, 6);
    assert!(after.hard_kills >= 3);
    eprintln!(
        "CLEAN:HARD = {:.2} ({} clean, {} hard), breaker rejections {}",
        after.clean_hard_ratio, after.clean_kills, after.hard_kills, after.breaker_rejections
    );

    cell.shutdown();
}

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
        "§2.5.8: a clean kill must run the guest's `finally`; logs were {:?}",
        logs.iter().map(|l| l.fields.clone()).collect::<Vec<_>>()
    );

    let hard = get(&cell, &module, "/spin");
    assert_eq!(hard.completion, Completion::BudgetKill);
    // Nothing to read back: the instance is a tombstone and its memory is a
    // state no execution ever produced. That refusal IS the observation.
    let slot = cell.slot(hard.instance).expect("addressable");
    assert!(slot.is_tombstoned());

    cell.shutdown();
}

#[test]
fn the_clean_hard_ratio_separates_a_cooperative_guest_from_a_spinning_one() {
    const N: usize = 12;

    for (label, route) in [("cooperative", "/cooperative"), ("spinning", "/spin")] {
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
        eprintln!(
            "CLEAN:HARD [{label} guest, N={N}, B={B_MS} ms]: {} clean, {} hard, ratio {:.1}; \
             {} tombstone(s), {:.3} ms recorded overage, {} grace deferral(s)",
            m.clean_kills,
            m.hard_kills,
            m.clean_hard_ratio,
            m.tombstones,
            m.overage_ns as f64 / 1e6,
            m.grace_deferrals,
        );

        if route == "/cooperative" {
            assert_eq!(m.clean_kills as usize, N);
            assert_eq!(m.hard_kills, 0, "a cooperative guest produced a hard kill");
            assert_eq!(m.tombstones, 0, "…and must never tombstone");
            // Every one of them stayed inside G, or "clean" would be a label
            // rather than a property.
            assert!(
                m.overage_ns < G_NS * N as u64,
                "average overage {:.3} ms exceeded G = {:.0} ms",
                m.overage_ns as f64 / 1e6 / N as f64,
                G_NS as f64 / 1e6
            );
        } else {
            assert_eq!(m.hard_kills as usize, N);
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

impl EmittedGuestLogs for wisp_host::bindings::InstanceCtx {
    fn emitted_logs(&self) -> Vec<TestGuestLog> {
        TestLogCapture::global().for_instance(self.instance)
    }
}
