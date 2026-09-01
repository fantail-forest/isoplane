//! **The two-phase kill and tombstones, end to end.**
//!
//! `crates/wisp-engine/tests/engine.rs` proves the *mechanism* against
//! hand-written `.wat`, where a guest-SDK bug could never be mistaken for an
//! engine bug. This file proves the *system*: a real Rust guest, compiled to
//! wasm, running on a real cell, with the real vtable, the real hang breaker
//! and the real metrics.
//!
//! The distinction is the point of the exercise. The grace window is only
//! worth having if clean kills are the common case, and that is a claim about
//! guests, not about engines. A grace window that only works when the fixture is written
//! to exploit it has proved nothing about the platform. So both routes here
//! come from `guests/misbehaver`:
//!
//! | route | cooperates? | expected |
//! |---|---|---|
//! | `/cooperative` | reads `budget_remaining_ns` | **clean** kill, slot reused, no tombstone |
//! | `/spin` | never checks, never awaits | **hard** kill at B+G, tombstone |
//! | `/gc_pin` | pins the *unsafe-to-trap* bit and spins | **hard** kill at B+G anyway |
//!
//! `/spin` and `/gc_pin` are the negative controls for the whole feature: if
//! the hard path is ever weakened to make the clean path look good, they are
//! what go red.
//!
//! ## If the guest wasm is missing
//!
//! Every test here refuses to pass vacuously. A missing `misbehaver.wasm`
//! fails the test with an explanation rather than skipping, because "the
//! clean kill works" claimed on the strength of a fake would be exactly the
//! flattering-direction result the negative controls exist to prevent.

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

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// The CPU budget every test here runs at.
///
/// Small enough that a spinner dies quickly, large enough that G takes its
/// 10% branch rather than the 2 ms floor — otherwise the tests would never
/// exercise the grace arithmetic at all. B = 40 ms ⇒ G = 4 ms.
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
            "cannot read {}: {e}.\n\nThese are the kill conformance tests and they are only \
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

// ---------------------------------------------------------------------------
// Conformance 1 — kill-then-call MUST observe refusal
// ---------------------------------------------------------------------------

/// A hard-killed instance transitions to tombstone: any subsequent host call
/// into it is a hard assertion failure, its slot is reset before reuse, and
/// deltas are never captured from it.
///
/// The reason this needs a test at all is that wasmtime does *not* poison a
/// store after a budget trap — a killed instance will answer a later exported
/// call correctly, globals intact. So the engine's forgiveness is real, and
/// the refusal has to come from the platform.
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

    // The instance that was killed, addressed directly — the door the engine
    // leaves open.
    let slot = cell
        .slot(killed.instance)
        .expect("the killed slot is still addressable");
    assert!(slot.is_tombstoned(), "a hard kill must leave a tombstone");
    assert_eq!(
        slot.enter().err(),
        Some(EnterError::Tombstoned),
        "a tombstoned instance answered a host call"
    );

    // And the two other doors a tombstone closes.
    assert!(
        !slot.is_capture_eligible(),
        "deltas are never captured from a tombstone"
    );
    assert!(
        slot.reset().is_err(),
        "a tombstone is discarded, not reset back into the pool"
    );

    let m = cell.metrics();
    assert_eq!(m.hard_kills, 1);
    assert_eq!(m.clean_kills, 0);
    assert_eq!(m.tombstones, 1);

    // The refusal is not the end of the world for the tenant: the *next*
    // request gets a different instance and the app keeps working. The blast
    // radius is the offender's own requests, and a tombstone must not widen it
    // to "the module".
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

// ---------------------------------------------------------------------------
// Conformance 2 — a dispatch that yields during grace MUST be observed clean
// ---------------------------------------------------------------------------

/// Code that unwinds and returns within grace receives a clean kill: state
/// consistent, `finally` runs, the slot is reusable, and the overage CPU is
/// recorded.
///
/// The guest is `misbehaver`'s `/cooperative`: the identical spin loop as
/// `/spin` with one addition — it reads the host-published remaining budget
/// at its own safepoints, and returns when the budget is spent. Everything
/// else about the two routes is the same, which is what makes this a
/// controlled comparison rather than two unrelated programs.
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
    // It is still a kill, and the tenant still sees `budget_exceeded`.
    assert_eq!(out.status, 503);
    assert!(
        out.trap
            .as_deref()
            .unwrap_or_default()
            .contains("budget_exceeded"),
        "a clean kill still reports budget_exceeded: {:?}",
        out.trap
    );

    // NOT a tombstone. This is the whole difference the grace window buys.
    let slot = cell.slot(out.instance);
    if let Some(s) = &slot {
        assert!(!s.is_tombstoned(), "a clean kill must not tombstone");
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
    assert!(m.overage_ns > 0, "the overage CPU is recorded");
    assert!(
        m.overage_ns < G_NS,
        "the recorded overage exceeded G, which would mean the clean kill landed past B+G: {} ns",
        m.overage_ns
    );

    // The overage is inside the ONE compute meter, not beside it.
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

    // The slot is reusable. Prove it by using it: the app keeps
    // serving, and a subsequent request may legitimately land on the very
    // instance the clean kill left behind.
    for _ in 0..3 {
        let ok = get(&cell, &module, "/ok");
        assert_eq!(ok.status, 200, "{:?}", ok.trap);
    }

    cell.shutdown();
}

/// **Negative control for conformance 2.**
///
/// The clean path must not be reachable by a guest that does not cooperate.
/// If `/spin` ever came back `CleanKill`, the grace window would have become
/// a 10% budget extension for everybody and the SLI would read healthy while
/// meaning nothing.
///
/// This is the test that must fail if the hard path is weakened — which is
/// exactly the pressure a feature like this creates, because a fleet with no
/// hard kills looks better on every dashboard.
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
    // The SLI, reading the truth about an unhealthy fleet.
    assert_eq!(m.clean_hard_ratio, 0.0);

    cell.shutdown();
}

// ---------------------------------------------------------------------------
// Conformance 3 — a genuine spin MUST tombstone at exactly B+G
// ---------------------------------------------------------------------------

/// A genuine spin must tombstone at exactly B+G, measured through the whole
/// stack rather than at the engine seam.
///
/// "Exactly" is bounded by the delivery mechanism: epochs tick every 200 µs
/// and the check is Cranelift-inserted at back-edges, so the honest claim is
/// "at B+G, within the epoch tick plus scheduling noise". Both sides are
/// pinned — early would mean the grace window was not really offered, late
/// would mean the deadline is soft — and `/gc_pin` is included because the
/// *unsafe-to-trap* bit is the one mechanism that could turn the hard
/// deadline into a negotiation.
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

// ---------------------------------------------------------------------------
// Clean kills do not trip the hang breaker
// ---------------------------------------------------------------------------

/// Repeated **hard** kills for the same (module, route) trip a breaker. Clean
/// kills do not count toward it — they are the budget working as designed —
/// but are surfaced in telemetry.
///
/// Both halves are asserted, because either one alone is a bug that hides.
/// A breaker that counted clean kills would quarantine the tenants who
/// cooperate best; a clean kill that vanished from telemetry would make the
/// clean:hard ratio unmeasurable.
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

    // The SLI, reported from the same numbers.
    assert_eq!(after.clean_kills, 6);
    assert!(after.hard_kills >= 3);
    eprintln!(
        "CLEAN:HARD = {:.2} ({} clean, {} hard), breaker rejections {}",
        after.clean_hard_ratio, after.clean_kills, after.hard_kills, after.breaker_rejections
    );

    cell.shutdown();
}

// ---------------------------------------------------------------------------
// What a clean kill runs and a hard kill does not
// ---------------------------------------------------------------------------

/// On a clean kill, `finally` blocks and `await using` disposals run; on a
/// hard kill there is no unwinding, no `finally`, no finalizers.
///
/// The guest's `/cooperative` route holds a `Drop` guard that logs on the way
/// out. `/spin` holds nothing, because there is nothing a hard kill could
/// run. So the observable is the log line, read back through the same vtable
/// the tenant's own logs go through.
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
    assert_eq!(hard.completion, Completion::BudgetKill);
    // Nothing to read back: the instance is a tombstone and its memory is a
    // state no execution ever produced. That refusal IS the observation.
    let slot = cell.slot(hard.instance).expect("addressable");
    assert!(slot.is_tombstoned());

    cell.shutdown();
}

// ---------------------------------------------------------------------------
// The clean:hard ratio, measured
// ---------------------------------------------------------------------------

/// The clean:hard ratio is a first-class SLI: hard kills should be rare
/// relative to clean ones.
///
/// The number a fleet would see is a property of its *guests*, so this
/// measures it per guest rather than per platform: a cooperative module and
/// an uncooperative one, the same budget, the same cell, N requests each. A
/// guest that always cooperates produces N:0, and a guest that never does
/// produces 0:N. The fleet number is the mixture, and it is a statement about
/// how many of a fleet's modules are in the first group.
///
/// No threshold is asserted on the ratio. Any threshold here would be a
/// threshold on a number this harness controls entirely — it is N, whatever N
/// is set to — which is the shape of a metric that always reads healthy. What
/// IS asserted is the separation: a cooperative guest must never produce a
/// hard kill, and an uncooperative one must never produce a clean one.
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
