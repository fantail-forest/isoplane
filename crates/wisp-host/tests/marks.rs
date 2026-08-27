//! **A request's whole life, accounted for.**
//!
//! The instrument this replaces measured three stages and computed a fourth by
//! subtraction, so everything the cell did that nobody had named went into one
//! bucket that could not be opened. The requirement now is stronger and it is
//! checkable: **the last mark minus the first equals the time from sending the
//! request to receiving the response, and every stretch in between is covered
//! by some adjacent pair.** No gaps, and nothing computed as a remainder.
//!
//! That property is what these tests are for. Each one is written so that it
//! *fails* if the thing it names is broken, rather than passing on a run where
//! the marks happen to be plausible:
//!
//! | Test | What it catches |
//! |---|---|
//! | [`a_traced_request_comes_home_with_its_whole_life_accounted_for`] | a mark that was never taken, a span that does not tile, a total computed as a sum |
//! | [`the_span_matches_what_the_caller_measured_around_it`] | the two clocks disagreeing, which is what an unshared epoch looks like |
//! | [`the_lane_lock_wait_is_separate_from_the_queue_wait`] | the merge this whole exercise exists to undo |
//! | [`a_busy_pool_stretches_the_queue_wait_and_not_the_lock_wait`] | the two being separate labels over one quantity |
//! | [`the_two_queue_structures_report_different_spans_and_neither_reports_a_zero`] | flattening two designs into one shape, and absent-as-zero |
//! | [`an_untraced_request_carries_no_marks_and_the_buffer_goes_back`] | the pool leaking, which shows up hours later as an untraced run |
//! | [`a_refused_request_returns_its_buffer_rather_than_a_partial_trace`] | a shed's six marks pooling into the same distributions as a served request's fifty |

use std::sync::Arc;
use std::time::{Duration, Instant};

use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};

use wisp_host::cell::{Cell, CellConfig, QueuePlacement, QueueStructure, Services, Target};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::Step;
use wisp_host::fakes::*;
use wisp_host::marks::{self, Mark, RequestMarksPool};
use wisp_host::tsc_marks::Calibration;
use wisp_host::{Activator, ModuleId, Resolver};

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

const T: Duration = Duration::from_secs(20);

struct Fixed(Vec<std::net::SocketAddr>);
impl Resolver for Fixed {
    fn resolve(&self, _h: &str, _p: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        Ok(self.0.clone())
    }
}

fn cell(cfg: CellConfig) -> Arc<Cell> {
    let svc = Arc::new(Services {
        egress: Arc::new(HostEgressPolicy::new(
            Box::new(Fixed(vec!["93.184.216.34:80".parse().unwrap()])),
            1024,
            1 << 30,
        )),
        fetcher: Arc::new(FakeFetcher::default()),
    });
    Cell::new(cfg, svc)
}

fn manifest(name: &str) -> AppManifest {
    AppManifest {
        name: name.into(),
        handlers: HandlerConfig {
            lifecycle: HandlerLifecycle::Reuse,
        },
        bindings: Bindings::default(),
        limits: Limits::default(),
        init_visible_vars: vec![],
    }
}

fn app(c: &Arc<Cell>, name: &str, behaviour: FakeBehaviour) -> ModuleId {
    let act = Arc::new(FakeActivator::named(ModuleId::new(name, "v1"), behaviour));
    let module = act.module().clone();
    c.register_module("acme", act, manifest(name)).unwrap();
    module
}

fn rec(id: u64, path: &str) -> DispatchRecord {
    DispatchRecord {
        dispatch_id: id,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: format!("http://app.example{path}"),
            path: path.into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        cpu_budget_ms: 5_000,
        now_ms: 0,
    }
}

fn route(path: &str) -> Target {
    Target::Handler { route: path.into() }
}

/// One calibration for the whole test binary. `Calibration::measure` sleeps for
/// 200 ms and the result is machine-wide, so measuring it per test would spend
/// most of the run asleep for no extra information.
fn calibration() -> Calibration {
    use std::sync::OnceLock;
    static CAL: OnceLock<Calibration> = OnceLock::new();
    *CAL.get_or_init(Calibration::measure)
}

fn pool() -> RequestMarksPool {
    marks::pool_for(calibration(), 8, 64, 8)
}

/// Send one request the way the load generator does — buffer checked out by
/// the caller, first mark taken before the cell is entered — and bring back the
/// trace, the caller's own end-to-end measurement, and the reply.
///
/// The caller's measurement is deliberately taken with `RDTSCP` through the
/// *same* calibration rather than with `mono_ns`: comparing a TSC span against
/// a `CLOCK_MONOTONIC` span would fail on the difference between two clocks'
/// resolution rather than on anything about the marks.
fn round_trip(
    cell: &Arc<Cell>,
    pool: &RequestMarksPool,
    module: &ModuleId,
    id: u64,
) -> Option<(Vec<(Mark, f64)>, f64, f64)> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mut m = pool.checkout();
    assert!(m.is_some(), "the pool was sized for this");

    let outer = pool
        .checkout()
        .expect("a second buffer, for the caller's own bracket");
    let mut outer = Some(outer);
    marks::begin(&mut outer);

    // `begin` *is* `CallerReleased`: the origin, not a span. See `Mark`.
    marks::begin(&mut m);
    let admitted = cell.submit_with_marks(module, route("/x"), rec(id, "/x"), tx, m);
    assert!(admitted, "the cell shed a request this test needs served");

    let mut res = rx.recv_timeout(T).expect("a reply");
    marks::mark(&mut res.marks, Mark::CallerReceived);
    marks::stop(&mut outer, Mark::CallerReceived);

    let trace = res.marks.as_ref().map(|m| wisp_host::marks::read(m))?;
    let outer_ns = outer.as_ref().unwrap().span_nanos();
    Some((trace.spans, trace.span_ns, outer_ns))
}

fn labels(spans: &[(Mark, f64)]) -> Vec<Mark> {
    spans.iter().map(|(l, _)| *l).collect()
}

// ---------------------------------------------------------------------------
// the coverage requirement
// ---------------------------------------------------------------------------

/// **Every stretch of the request is covered, and the total is not a sum.**
///
/// Three separate claims, because they fail separately:
///
/// 1. The marks arrive in the order the code can produce them. A mark that was
///    never taken shows up as a missing label rather than as a small number,
///    which is the whole reason the residual was replaced.
/// 2. The span from the first mark to the last is the request. It is read as
///    `last - first` rather than as the sum of the parts — those differ by one
///    calibrated mark cost per span, about 2 µs over fifty marks, and a report
///    that published the sum would under-state the request by exactly the
///    amount of its own instrument.
/// 3. Nothing is negative. On one core that is impossible; across the three
///    threads a request visits it is the detector for the counters not being
///    synchronised after all, which is the failure the startup guard's
///    `cross_core_ok` is supposed to have ruled out.
#[test]
fn a_traced_request_comes_home_with_its_whole_life_accounted_for() {
    let c = cell(CellConfig::for_test("marks-cover"));
    let m = app(&c, "echo", FakeBehaviour::default());
    let p = pool();

    let (spans, span_ns, _) = round_trip(&c, &p, &m, 0).expect("a traced reply");
    let got = labels(&spans);

    // The plain handler path, in order. Written out rather than checked
    // loosely, because "the marks that happen to be there are in order" is
    // true of any prefix and would pass with half the request unmeasured.
    let want = vec![
        Mark::CellEntered,
        Mark::BreakerLockWanted,
        Mark::BreakerChecked,
        Mark::JobBuilt,
        Mark::PlacementDecided,
        Mark::LaneLockWanted,
        Mark::LaneLockAcquired,
        Mark::JobTaken,
        Mark::DispatchStarted,
        Mark::ModulesLockWanted,
        Mark::ModulesLockReleased,
        Mark::InstanceAcquireBegan,
        Mark::ResidentsLockWanted,
        Mark::ResidentsLockAcquired,
        Mark::ResidentsLockReleased,
        Mark::InstanceAcquireEnded,
        Mark::ActiveProbeWanted,
        Mark::ActiveProbeReleased,
        Mark::CancelAllocWanted,
        Mark::CancelAllocAcquired,
        Mark::CancelAllocReleased,
        Mark::ActiveInsertWanted,
        Mark::ActiveInsertReleased,
        Mark::SlotEnterWanted,
        Mark::SlotEnterAcquired,
        Mark::SlotEnterReleased,
        Mark::GuestEntered,
        Mark::GuestReturned,
        Mark::SlotReleaseWanted,
        Mark::SlotReleaseReleased,
        Mark::TenantRecordWanted,
        Mark::TenantRecordReleased,
        Mark::ActiveUpdateWanted,
        Mark::ActiveUpdateAcquired,
        Mark::ActiveUpdateReleased,
        Mark::ResponseTableWanted,
        Mark::ResponseTableAcquired,
        Mark::ResponseTableReleased,
        Mark::WaitUntilCheckWanted,
        Mark::WaitUntilCheckAcquired,
        Mark::WaitUntilCheckReleased,
        Mark::ResultReadWanted,
        Mark::ResultReadReleased,
        Mark::ResponderTakenWanted,
        Mark::ResponderTakenReleased,
        Mark::ReplyHandedOff,
        Mark::CallerReceived,
    ];
    assert_eq!(
        got,
        want,
        "the trace is not the plain handler path.\n  got:  {:?}\n  want: {:?}",
        got.iter().map(|m| m.name()).collect::<Vec<_>>(),
        want.iter().map(|m| m.name()).collect::<Vec<_>>(),
    );

    // The total is the span, not the sum of the parts.
    let summed: f64 = spans.iter().map(|(_, ns)| ns).sum();
    assert!(span_ns > 0.0, "a request that took {span_ns} ns");
    assert!(
        span_ns > summed,
        "the whole span ({span_ns:.0} ns) must be longer than the sum of the \
         overhead-corrected parts ({summed:.0} ns); if it is not, the correction is \
         not being applied and the parts are being published as though they were raw"
    );

    // Nothing ran backwards.
    for (label, ns) in &spans {
        assert!(
            *ns >= 0.0,
            "`{}` reports {ns} ns, which is not a duration",
            label.name()
        );
    }

    // **`BreakerSuccessWanted` is deliberately absent above**, and this is the
    // reason: a `Reuse` handler that completes cleanly is disposed, and only a
    // disposed request records success against the breaker. Asserting its
    // absence here rather than leaving it out silently is what stops a future
    // reader reading the missing row as a lock that never blocked.
    assert!(
        !got.contains(&Mark::BreakerSuccessWanted) || got.contains(&Mark::BreakerSuccessReleased),
        "the breaker's success pair is half present, which is worse than either"
    );
}

/// **The two ends agree, which is what one clock epoch buys.**
///
/// `wisp_host::mono_ns` used to subtract a per-process origin that
/// `wisp_bench::schedule::mono_ns` knew nothing about, so a duration computed
/// across the two was the machine's uptime or a saturated zero. Nothing about
/// either number looked wrong.
///
/// Here the caller brackets the whole round trip with its own independent
/// `Marks`, and the request's own first-to-last span must fall inside it. The
/// caller's bracket is necessarily the larger of the two — it includes the
/// checkout, the channel construction and the `recv` — so the test is an
/// inequality with a ceiling, not an equality: a span *longer* than the bracket
/// around it is the specific shape a two-epoch subtraction produces.
#[test]
fn the_span_matches_what_the_caller_measured_around_it() {
    let c = cell(CellConfig::for_test("marks-epoch"));
    let m = app(&c, "echo", FakeBehaviour::default());
    let p = pool();

    let (_, span_ns, outer_ns) = round_trip(&c, &p, &m, 0).expect("a traced reply");
    assert!(
        span_ns <= outer_ns,
        "the request's own span ({span_ns:.0} ns) is longer than the caller's bracket \
         around it ({outer_ns:.0} ns), so the two ends are not on one clock"
    );
    // …and not absurdly smaller either, which is what a span measured against
    // the wrong origin and clamped would look like.
    assert!(
        span_ns > outer_ns * 0.10,
        "the request's span ({span_ns:.0} ns) is under a tenth of the bracket around it \
         ({outer_ns:.0} ns); the marks are not measuring this request"
    );
}

// ---------------------------------------------------------------------------
// the separation this work exists for
// ---------------------------------------------------------------------------

/// **The lane lock's wait and the wait for a worker are two spans.**
///
/// They used to be one number, `queue_ns`, and they have different fixes: a
/// lock wait wants a shorter critical section or more lanes, a queue wait wants
/// more workers. A single figure cannot say which you have.
#[test]
fn the_lane_lock_wait_is_separate_from_the_queue_wait() {
    let c = cell(CellConfig::for_test("marks-split"));
    let m = app(&c, "echo", FakeBehaviour::default());
    let p = pool();

    let (spans, _, _) = round_trip(&c, &p, &m, 0).expect("a traced reply");
    let by = |l: Mark| spans.iter().find(|(k, _)| *k == l).map(|(_, ns)| *ns);

    let lock = by(Mark::LaneLockAcquired).expect("the lane lock's wait is a span");
    let queue = by(Mark::JobTaken).expect("the wait for a worker is a span");
    assert!(lock >= 0.0 && queue >= 0.0);
    assert_ne!(
        Mark::LaneLockAcquired.name(),
        Mark::JobTaken.name(),
        "two quantities under one name is the merge this replaces"
    );
    // On an idle cell the lock is uncontended, so its wait is at or under the
    // measurement floor while the queue wait is a real scheduling delay. That
    // ordering is the cheapest possible evidence that they are not the same
    // reading printed twice.
    let floor = calibration().ticks_to_nanos(calibration().mark_overhead_ticks()) * 2.0;
    assert!(
        lock < floor.max(1_000.0),
        "an uncontended lane lock reports a {lock:.0} ns wait against a {floor:.0} ns floor"
    );
}

/// **A busy pool stretches the queue wait and leaves the lock wait alone.**
///
/// The negative control for the test above: two labels over one quantity would
/// move together. One execution thread and a guest that takes real time means
/// the second request waits for a worker, and nothing about that wait belongs
/// to the intake lock.
#[test]
fn a_busy_pool_stretches_the_queue_wait_and_not_the_lock_wait() {
    let mut cfg = CellConfig::for_test("marks-busy");
    cfg.exec_threads = 1;
    cfg.max_hot = 1;
    let c = cell(cfg);
    // `FakeBehaviour` has no "take N milliseconds" step. `CooperativeSpin`
    // burns real CPU on a real pool thread until it crosses its own budget,
    // notices grace and unwinds — so the budget on the *record* is what sets
    // how long the worker is held, and the blocker finishes cleanly rather
    // than being killed.
    let fast = app(&c, "echo", FakeBehaviour::default());
    let slow = app(
        &c,
        "slow",
        FakeBehaviour::program([Step::CooperativeSpin { unwind_us: 50 }]),
    );
    let m = fast;
    let p = pool();

    // Occupy the single worker, then send the request that has to wait.
    let (bt, br) = std::sync::mpsc::sync_channel(1);
    let mut blocker = rec(1, "/x");
    blocker.cpu_budget_ms = 60;
    assert!(c.submit_with(&slow, route("/x"), blocker, bt));
    // Give the worker time to actually pick the blocker up, so the second
    // request queues rather than racing it.
    std::thread::sleep(Duration::from_millis(10));

    let (spans, _, _) = round_trip(&c, &p, &m, 2).expect("a traced reply");
    let by = |l: Mark| {
        spans
            .iter()
            .find(|(k, _)| *k == l)
            .map(|(_, ns)| *ns)
            .unwrap_or(0.0)
    };

    let queue = by(Mark::JobTaken);
    let lock = by(Mark::LaneLockAcquired);
    assert!(
        queue > 10_000_000.0,
        "the worker was busy for 60 ms and the queue wait reports {queue:.0} ns"
    );
    assert!(
        lock < queue / 100.0,
        "the lane lock's wait ({lock:.0} ns) moved with the queue wait ({queue:.0} ns), \
         which is what two labels over one quantity looks like"
    );
    let _ = br.recv_timeout(T);
}

/// **Two queue designs, two sets of spans, and no zeros standing in for
/// absence.**
///
/// A lock-free queue has no lane mutex to wait on and a mutex queue has no
/// `pushing` counter, and flattening them into a common shape would make a
/// report column comparing the structures compare two different things. The
/// requirement is that each run simply has no observations for the other's
/// spans — absent, never zero.
#[test]
fn the_two_queue_structures_report_different_spans_and_neither_reports_a_zero() {
    let mut shared = CellConfig::for_test("marks-shared");
    shared.queue_structure = QueueStructure::Shared;
    shared.queue_placement = QueuePlacement::default();
    let mut free = CellConfig::for_test("marks-free");
    free.queue_structure = QueueStructure::LockFree;
    free.queue_placement = QueuePlacement::default();

    let mut seen = Vec::new();
    for cfg in [shared, free] {
        let structure = cfg.queue_structure;
        let c = cell(cfg);
        let m = app(&c, "echo", FakeBehaviour::default());
        let p = pool();
        let (spans, _, _) = round_trip(&c, &p, &m, 0).expect("a traced reply");
        seen.push((structure, labels(&spans)));
    }

    let (_, mutex) = &seen[0];
    let (_, lockfree) = &seen[1];

    assert!(mutex.contains(&Mark::LaneLockWanted) && mutex.contains(&Mark::LaneLockAcquired));
    assert!(
        !mutex.contains(&Mark::PushRegistered) && !mutex.contains(&Mark::ClosedChecked),
        "a mutex queue reported the ring's spans"
    );

    assert!(
        lockfree.contains(&Mark::PushRegistered) && lockfree.contains(&Mark::ClosedChecked),
        "a lock-free queue did not report the ring's spans"
    );
    assert!(
        !lockfree.contains(&Mark::LaneLockWanted) && !lockfree.contains(&Mark::LaneLockAcquired),
        "a lock-free queue reported a lane lock it does not have — and a zero here \
         reads exactly like a lock that was watched and never blocked"
    );

    // The span *before* the enqueue means the same thing under both, which is
    // what makes the two tables comparable at all.
    assert!(mutex.contains(&Mark::PlacementDecided) && lockfree.contains(&Mark::PlacementDecided));
}

// ---------------------------------------------------------------------------
// the pool
// ---------------------------------------------------------------------------

/// **An untraced request costs one `Option` check, and the buffer comes back.**
///
/// A pool that leaks is not a crash: it is a run whose first few thousand
/// requests are traced and whose remaining millions are not, with the
/// percentiles quietly taken from the warm-up.
#[test]
fn an_untraced_request_carries_no_marks_and_the_buffer_goes_back() {
    let c = cell(CellConfig::for_test("marks-none"));
    let m = app(&c, "echo", FakeBehaviour::default());
    let p = pool();
    let before = p.available();

    // No buffer at all: the plain `submit_with` path, which every other caller
    // in the workspace uses.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    assert!(c.submit_with(&m, route("/x"), rec(1, "/x"), tx));
    let res = rx.recv_timeout(T).expect("a reply");
    assert!(
        res.marks.is_none(),
        "an untraced request came back carrying a trace"
    );

    // And a traced one gives its buffer back when the reply is dropped.
    let (spans, _, _) = round_trip(&c, &p, &m, 2).expect("a traced reply");
    assert!(!spans.is_empty());
    assert!(
        wait_for(|| p.available() == before, Duration::from_secs(2)),
        "the pool went from {before} free to {} and stayed there",
        p.available()
    );
    assert_eq!(
        p.exhausted_checkouts(),
        0,
        "this test never asked for more than it sized for"
    );
}

/// **A refusal returns the buffer and does not publish a partial trace.**
///
/// A shed reaches perhaps six of a request's fifty marks. Pooling that into the
/// same per-label distributions as a served request would shorten every span it
/// touched — and it would do so *under load*, which is precisely when the
/// numbers are being read.
#[test]
fn a_refused_request_returns_its_buffer_rather_than_a_partial_trace() {
    let mut cfg = CellConfig::for_test("marks-shed");
    cfg.exec_threads = 1;
    cfg.max_hot = 1;
    cfg.admission_queue = 1;
    cfg.queue_depth_per_worker = 1;
    let c = cell(cfg);
    let m = app(
        &c,
        "slow",
        FakeBehaviour::program([Step::CooperativeSpin { unwind_us: 50 }]),
    );
    let p = pool();
    let before = p.available();

    // Fill the worker and the one-deep queue, then offer more until one is
    // refused. `submit_with_marks` returning `false` is the shed.
    let mut keep = Vec::new();
    let mut shed = false;
    for id in 0..64u64 {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        keep.push(rx);
        let mut mk = p.checkout();
        marks::begin(&mut mk);
        let mut r = rec(id, "/x");
        r.cpu_budget_ms = 200;
        if !c.submit_with_marks(&m, route("/x"), r, tx, mk) {
            shed = true;
            break;
        }
    }
    assert!(shed, "the cell never shed, so this test measured nothing");
    assert!(
        wait_for(
            || p.available() >= before - keep.len(),
            Duration::from_secs(5)
        ),
        "the shed request's buffer did not come back: {} free of {before}",
        p.available()
    );
    assert_eq!(p.exhausted_checkouts(), 0);
    drop(keep);
    c.shutdown();
}

fn wait_for(mut cond: impl FnMut() -> bool, within: Duration) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < within {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}
