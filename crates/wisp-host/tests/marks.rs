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
/// A host timer long enough that it never fires on its own, so a test that
/// parks on it decides for itself when the wake happens.
const PARK_MS: u64 = 600_000;

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
    let m = pool.checkout();
    assert!(m.is_some(), "the pool was sized for this");

    let outer = Some(
        pool.checkout()
            .expect("a second buffer, for the caller's own bracket"),
    );
    marks::begin(&outer);

    // `begin` *is* `CallerReleased`: the origin, not a span. See `Mark`.
    marks::begin(&m);
    let out = cell.submit_with_marks(module, route("/x"), rec(id, "/x"), tx, m);
    assert!(
        out.admitted,
        "the cell shed a request this test needs served"
    );

    let res = rx.recv_timeout(T).expect("a reply");
    marks::mark(&res.marks, Mark::CallerReceived);
    marks::stop(&outer, Mark::CallerReceived);

    let trace = res.marks.as_ref().map(wisp_host::marks::read)?;
    let outer_ns = wisp_host::marks::read(outer.as_ref().unwrap()).span_ns;
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

    // Request 0 activates: nothing is in the pool yet, so this trace carries
    // the cold path's marks as well.
    let (spans, span_ns, _) = round_trip(&c, &p, &m, 0).expect("a traced reply");
    let got = labels(&spans);

    // The plain handler path, in order. Written out rather than checked
    // loosely, because "the marks that happen to be there are in order" is
    // true of any prefix and would pass with half the request unmeasured.
    let want = vec![
        Mark::CellEntered,
        Mark::BreakerLockWanted,
        Mark::BreakerLockAcquired,
        Mark::BreakerChecked,
        Mark::JobBuilt,
        Mark::PlacementDecided,
        Mark::LaneLockWanted,
        Mark::LaneLockAcquired,
        // The producer's tail, written after the job was handed over — the
        // three spans a single-owner buffer had to merge away.
        Mark::LaneLockReleased,
        Mark::WaiterNotified,
        Mark::JobTaken,
        Mark::DispatchStarted,
        Mark::ModulesLockWanted,
        Mark::ModulesLockAcquired,
        Mark::ModulesLockReleased,
        Mark::InstanceAcquireBegan,
        // Getting a sandbox, subdivided. `acquire.rest` used to be all of
        // this: consulting the idle lists, building a new instance, and the
        // index insert are three different costs and the middle one is three
        // orders of magnitude larger than the other two.
        Mark::PoolLockWanted,
        Mark::PoolLockReleased,
        Mark::ActivationBegan,
        Mark::ActivationEnded,
        Mark::IndexLockWanted,
        Mark::IndexLockReleased,
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
        Mark::TenantRecordAcquired,
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
        // Time the cell was blocked on a full reply channel, which used to be
        // inside the reply's flight and indistinguishable from it.
        Mark::SendReturned,
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

    // **The trace ends at the reply, and that is the boundary.** Everything
    // the cell does after answering — recording success against the breaker,
    // releasing the instance's futures, returning the sandbox to its pool —
    // is past `ReplyHandedOff` and is not on this trace, because the trace
    // travels inside the reply. `reply.delivery` is therefore the last span
    // the cell contributes to, and the only mark after it is the caller's own.
    let after_handoff: Vec<Mark> = got
        .iter()
        .skip_while(|m| **m != Mark::ReplyHandedOff)
        .skip(1)
        .copied()
        .collect();
    assert_eq!(
        after_handoff,
        vec![Mark::SendReturned, Mark::CallerReceived],
        "something the cell does after answering ended up inside the request's own span"
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
// pausing and resuming
// ---------------------------------------------------------------------------

/// **A request that pauses keeps its trace, and every pass is recorded.**
///
/// This is the half the old instrument could not see at all. `guest_ns` timed
/// the first slice only, so a guest that awaited three times reported the guest
/// time of one third of itself — and the stretch where it sat parked waiting on
/// the outside world was inside the residual, unnamed and unbounded. A parking
/// guest therefore reported as the fastest thing in the campaign.
///
/// Three claims:
///
/// 1. The trace **survives the park**. It lives on the execution thread while a
///    thread is running the request and there is no thread while it is parked,
///    so it is handed to `Cell::active` and reclaimed by whichever worker picks
///    the wake up. Losing it there would silently truncate every parking
///    request to its first slice — which is exactly what the old code did.
/// 2. The wait for the outside world is **its own span**, separated from the
///    wake's trip through the priority lane.
/// 3. The guest span appears **once per pass**, not once per request.
#[test]
fn a_parking_request_keeps_its_trace_across_the_pause() {
    let c = cell(CellConfig::for_test("marks-park"));
    // One park on a host timer long enough that it will not fire on its own,
    // so this test decides when the wake happens rather than racing a fake.
    let m = app(
        &c,
        "parker",
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", PARK_MS)],
        }]),
    );
    let p = pool();

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mk = p.checkout();
    marks::begin(&mk);
    assert!(
        c.submit_with_marks(&m, route("/x"), rec(0, "/x"), tx, mk)
            .admitted
    );

    // Wait for the instance to actually be parked before completing, so the
    // request really does go dormant rather than the completion racing the
    // park and being drained in place.
    assert!(
        wait_for(|| c.metrics().parked > 0, T),
        "the guest never parked, so nothing about resuming was measured"
    );
    // The one host future the guest is waiting on.
    let fid = c
        .futures()
        .live_future_ids()
        .into_iter()
        .next()
        .expect("one pending future");
    std::thread::sleep(Duration::from_millis(20));
    c.complete_future(fid, wisp_abi::result::ResultRecord::Ok(vec![]));

    let res = rx.recv_timeout(T).expect("a reply after the wake");
    marks::mark(&res.marks, Mark::CallerReceived);
    let t = wisp_host::marks::read(res.marks.as_ref().expect("the trace survived the park"));
    let got: Vec<Mark> = t.spans.iter().map(|(l, _)| *l).collect();
    let names = || got.iter().map(|m| m.name()).collect::<Vec<_>>();

    // 1. It survived.
    assert!(
        got.contains(&Mark::SliceEnded),
        "the park was never marked: {:?}",
        names()
    );
    assert!(
        got.contains(&Mark::WakeStarted),
        "the trace did not survive the park — it would have ended at the first slice: {:?}",
        names()
    );

    // 2. The two halves of the pause are separate spans.
    let ns = |l: Mark| {
        t.spans
            .iter()
            .find(|(k, _)| *k == l)
            .map(|(_, ns)| *ns)
            .unwrap_or(-1.0)
    };
    let outside = ns(Mark::WakeEnqueueBegan);
    let lane = ns(Mark::WakeStarted);
    assert!(
        outside >= 0.0 && lane >= 0.0,
        "both halves must be present: {:?}",
        names()
    );
    assert!(
        outside > 15_000_000.0,
        "the guest was left parked for 20 ms and the wait for the outside world reports \
         {outside:.0} ns"
    );
    assert!(
        lane < outside / 10.0,
        "the priority lane's wait ({lane:.0} ns) moved with the park ({outside:.0} ns), which \
         is what one span wearing two names looks like"
    );

    // 3. The guest ran twice and both passes are on the trace.
    assert_eq!(
        got.iter().filter(|l| **l == Mark::GuestReturned).count(),
        2,
        "one dispatch and one wake is two passes through the guest: {:?}",
        names()
    );
    assert_eq!(
        got.iter()
            .filter(|l| **l == Mark::SlotEnterReleased)
            .count(),
        2,
        "the sandbox is entered once per pass"
    );
    // The resume's own locks are named apart from the dispatch path's, so a
    // row cannot pool two different contention regimes on the same lock.
    assert!(got.contains(&Mark::WakeResidentsLockAcquired));
    assert!(got.contains(&Mark::TakeReadyAcquired));

    // And the whole thing still tiles: the total is the whole pause, not one
    // slice of it.
    assert!(
        t.span_ns > 15_000_000.0,
        "a request that was parked for 20 ms reports a total of {:.0} ns",
        t.span_ns
    );
    assert!(!t.truncated, "one park cannot fill the buffer");
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
        let mk = p.checkout();
        marks::begin(&mk);
        let mut r = rec(id, "/x");
        r.cpu_budget_ms = 200;
        let out = c.submit_with_marks(&m, route("/x"), r, tx, mk);
        if !out.admitted {
            // **The shed's trace comes back through the return value**, which
            // is the only channel a shed has: `submit_with` deliberately sends
            // nothing on the reply channel, so a caller draining one shared
            // receiver cannot count the same refusal twice.
            let t = out.refused_marks.expect("a shed hands its trace back");
            let read = wisp_host::marks::read(&t);
            assert!(
                read.spans.iter().any(|(l, _)| *l == Mark::ShedDecided),
                "a shed trace must say it was shed: {:?}",
                read.spans.iter().map(|(l, _)| l.name()).collect::<Vec<_>>()
            );
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

// ---------------------------------------------------------------------------
// getting a sandbox
// ---------------------------------------------------------------------------

/// An activator that takes a visible amount of time to build an instance.
///
/// The fake one is a `Vec` clone and a struct literal, so against it an
/// activation span and a lock span are the same size and a test asserting
/// "the activation is separated out" would pass on a report that had silently
/// folded it back into the remainder. Ten milliseconds cannot hide anywhere.
struct SlowActivator {
    inner: Arc<FakeActivator>,
    delay: Duration,
}

impl Activator for SlowActivator {
    fn module(&self) -> &ModuleId {
        self.inner.module()
    }
    fn module_hash(&self) -> wisp_engine::ModuleHash {
        self.inner.module_hash()
    }
    fn abi_rev(&self) -> u32 {
        self.inner.abi_rev()
    }
    fn template_image(&self) -> Arc<Vec<u8>> {
        self.inner.template_image()
    }
    fn activate(
        &self,
        bindings: Box<dyn wisp_engine::HostBindings>,
    ) -> anyhow::Result<Box<dyn wisp_engine::InstanceOps + Send>> {
        std::thread::sleep(self.delay);
        self.inner.activate(bindings)
    }
}

/// **The activation is its own span, and a warm request does not pay it.**
///
/// `acquire.rest` used to be the warm-pool pop, the hibernation wake *and* the
/// activation. Those differ by three orders of magnitude, so one row over all
/// three describes a distribution with no mode — and the p99 of a cell serving
/// a million warm requests and a hundred cold ones is entirely the hundred.
///
/// Two claims, and they fail separately:
///
/// 1. A cold request reports `activation` and a warm one does not report it at
///    all. Absent, not zero — a zero here would read exactly like an
///    activation that took no time.
/// 2. The ten milliseconds land **in** `activation` and not in `acquire.rest`
///    or `pool.idle`. Without the second half the first would pass on an
///    instrument that emitted the label and measured the wrong interval.
#[test]
fn the_activation_is_its_own_span_and_a_warm_request_does_not_pay_it() {
    let c = cell(CellConfig::for_test("marks-activate"));
    let act = Arc::new(SlowActivator {
        inner: Arc::new(FakeActivator::named(
            ModuleId::new("slow", "v1"),
            FakeBehaviour::default(),
        )),
        delay: Duration::from_millis(10),
    });
    let m = act.module().clone();
    c.register_module("acme", act, manifest("slow")).unwrap();
    let p = pool();

    let (cold, _, _) = round_trip(&c, &p, &m, 0).expect("a traced reply");
    // The instance goes back to the pool in the cell's tail, *after* the reply
    // was sent, so the second request has to wait for it or it activates too.
    assert!(
        wait_for(
            || c.idle_instances(&m).map(|(w, f)| w + f).unwrap_or(0) > 0,
            T
        ),
        "the instance never returned to its pool, so there was no warm request to compare"
    );
    let (warm, _, _) = round_trip(&c, &p, &m, 1).expect("a traced reply");

    let span =
        |spans: &[(Mark, f64)], l: Mark| spans.iter().find(|(k, _)| *k == l).map(|(_, ns)| *ns);

    // 1. Present on the cold path, absent on the warm one.
    let activation = span(&cold, Mark::ActivationEnded).expect("the cold request built a sandbox");
    assert!(
        span(&warm, Mark::ActivationEnded).is_none(),
        "a warm request reported an activation span; a row here is indistinguishable from an \
         activation that cost nothing"
    );
    // Both consult the pool, and both take the index lock.
    for (which, spans) in [("cold", &cold), ("warm", &warm)] {
        assert!(
            span(spans, Mark::PoolLockReleased).is_some(),
            "{which}: no pool consult"
        );
        assert!(
            span(spans, Mark::IndexLockReleased).is_some(),
            "{which}: no index insert"
        );
    }

    // 2. The ten milliseconds are in the activation and nowhere else.
    assert!(
        activation > 8_000_000.0,
        "the activator slept for 10 ms and `activation` reports {activation:.0} ns — the span \
         does not bracket the engine call"
    );
    for l in [
        Mark::InstanceAcquireEnded,
        Mark::PoolLockReleased,
        Mark::ActivationBegan,
    ] {
        let leaked = span(&cold, l).unwrap_or(0.0);
        assert!(
            leaked < activation / 10.0,
            "`{}` reports {leaked:.0} ns against an activation of {activation:.0} ns, so the \
             activation is still being counted somewhere it is not named",
            l.name()
        );
    }
}

// ---------------------------------------------------------------------------
// what a refusal costs
// ---------------------------------------------------------------------------

/// **A shed pays for the module table, and the request had not touched it.**
///
/// The refusal was marked at the decision point and the work it then does was
/// not. That work is not nothing: recording the shed as guest work needs
/// `Meters`, which lives behind the module table's read lock — a lock this
/// request never otherwise acquires, because it is refused inside
/// `submit_with_marks` and never reaches `run_dispatch`'s own read of it.
///
/// So the claim is a **sequence**, not a presence: `refused.shed` is followed
/// by the three `shed.meters.*` marks and by nothing else, which is what
/// catches a mark emitted around the wrong interval or in the wrong order.
#[test]
fn a_shed_pays_for_the_module_table_and_the_span_says_so() {
    let mut cfg = CellConfig::for_test("marks-shed-cost");
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

    let mut keep = Vec::new();
    let mut shed = None;
    for id in 0..64u64 {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        keep.push(rx);
        let mk = p.checkout();
        marks::begin(&mk);
        let mut r = rec(id, "/x");
        r.cpu_budget_ms = 200;
        let out = c.submit_with_marks(&m, route("/x"), r, tx, mk);
        if !out.admitted {
            shed = out.refused_marks.map(|t| wisp_host::marks::read(&t));
            break;
        }
    }
    let t = shed.expect("the cell never shed, so this test measured nothing");
    let got: Vec<Mark> = t.spans.iter().map(|(l, _)| *l).collect();
    let tail: Vec<Mark> = got
        .iter()
        .skip_while(|l| **l != Mark::ShedDecided)
        .copied()
        .collect();
    assert_eq!(
        tail,
        vec![
            Mark::ShedDecided,
            Mark::ShedMetersLockWanted,
            Mark::ShedMetersLockAcquired,
            Mark::ShedMetersLockReleased,
        ],
        "a shed's tail is not the module-table read it actually does: {:?}",
        got.iter().map(|l| l.name()).collect::<Vec<_>>()
    );
    // And it is the *only* acquisition of that table on this trace, under its
    // own label — a shed never reaches `modules.read.*`.
    assert!(
        !got.contains(&Mark::ModulesLockAcquired),
        "a shed reported the dispatch path's module read, so the two labels have been merged"
    );
    drop(keep);
    c.shutdown();
}

#[test]
fn a_trap_reports_the_tombstone_ring_and_never_touches_the_breaker() {
    let c = cell(CellConfig::for_test("marks-trap"));
    let m = app(
        &c,
        "trapper",
        FakeBehaviour::program([Step::Trap(wisp_abi::status::TrapCode::Panic)]),
    );
    let p = pool();
    let (spans, _, _) = round_trip(&c, &p, &m, 0).expect("a traced reply");
    let got = labels(&spans);
    let names = || got.iter().map(|l| l.name()).collect::<Vec<_>>();

    // The three marks, in order, and between the `active` update and the
    // result read — where the tombstone bookkeeping actually happens.
    let window: Vec<Mark> = got
        .iter()
        .skip_while(|l| **l != Mark::ActiveUpdateReleased)
        .take_while(|l| **l != Mark::ResultReadWanted)
        .copied()
        .collect();
    assert_eq!(
        window,
        vec![
            Mark::ActiveUpdateReleased,
            Mark::TombstoneRingWanted,
            Mark::TombstoneRingAcquired,
            Mark::TombstoneRingReleased,
        ],
        "the trap's tail is not the tombstone ring it actually takes: {:?}",
        names()
    );
    assert!(
        !got.contains(&Mark::BreakerKillWanted),
        "a trap recorded a kill against the hang breaker; §6.4 says only hard kills do: {:?}",
        names()
    );
    c.shutdown();
}

/// **A hard kill takes the breaker as well, and it is the second time.**
///
/// The same request already waited for `HangBreaker::entries` once, at
/// admission, tens of microseconds and a whole dispatch earlier. Recording the
/// kill goes back to it under entirely different contention, and the hold
/// clones a `BreakerKey` — three `String` allocations — inside the lock before
/// hashing it. Sharing `breaker.check.*` with the admission read would pool
/// two acquisitions that have nothing in common but the mutex.
#[test]
fn a_hard_kill_takes_the_breaker_a_second_time_under_its_own_name() {
    let mut cfg = CellConfig::for_test("marks-hardkill");
    cfg.wall_ms_cap = 5_000;
    let c = cell(cfg);
    let m = app(&c, "spinner", FakeBehaviour::program([Step::Spin]));
    let p = pool();

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mk = p.checkout();
    marks::begin(&mk);
    let mut r = rec(0, "/x");
    r.cpu_budget_ms = 20;
    assert!(c.submit_with_marks(&m, route("/x"), r, tx, mk).admitted);
    let res = rx.recv_timeout(T).expect("the spinner was never killed");
    assert_eq!(
        res.status, 503,
        "this dispatch was not hard-killed: {:?}",
        res.trap
    );
    marks::mark(&res.marks, Mark::CallerReceived);
    let t = wisp_host::marks::read(res.marks.as_ref().expect("a trace"));
    let got: Vec<Mark> = t.spans.iter().map(|(l, _)| *l).collect();
    let names = || got.iter().map(|l| l.name()).collect::<Vec<_>>();

    let window: Vec<Mark> = got
        .iter()
        .skip_while(|l| **l != Mark::ActiveUpdateReleased)
        .take_while(|l| **l != Mark::ResultReadWanted)
        .copied()
        .collect();
    assert_eq!(
        window,
        vec![
            Mark::ActiveUpdateReleased,
            Mark::BreakerKillWanted,
            Mark::BreakerKillAcquired,
            Mark::BreakerKillReleased,
            Mark::TombstoneRingWanted,
            Mark::TombstoneRingAcquired,
            Mark::TombstoneRingReleased,
        ],
        "a hard kill's refusal work is not what the code does: {:?}",
        names()
    );
    // Two acquisitions of one mutex in one request, under two names. Pooling
    // them would average an uncontended admission check with a kill taken
    // while the cell is full of spinners.
    assert_eq!(
        got.iter()
            .filter(|l| **l == Mark::BreakerLockAcquired)
            .count(),
        1,
        "the admission check must still be exactly one observation of its own label"
    );
    c.shutdown();
}

// ---------------------------------------------------------------------------
// the cell's tail
// ---------------------------------------------------------------------------

/// A sink that keeps every tail it is handed.
#[derive(Default)]
struct CollectTails(std::sync::Mutex<Vec<wisp_host::marks::Trace>>);

impl wisp_host::marks::TailSink for CollectTails {
    fn record(&self, t: &wisp_host::marks::Trace) {
        self.0.lock().unwrap().push(t.clone());
    }
}

impl CollectTails {
    fn takeall(&self) -> Vec<wisp_host::marks::Trace> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// **The cell's tail is measured, on a trace of its own, and it never touches
/// the request's.**
///
/// After `Cell::respond` the request's buffer is inside the reply and gone
/// from this thread, but the worker is not finished: it discards the outbox,
/// removes the `active` entry, reaps the dispatch's futures, scans the whole
/// cell's future table to release the instance's handles and returns the
/// sandbox to its pool. That work holds an execution thread and is therefore
/// capacity — it is simply not the request's latency, because the caller
/// already has its answer.
///
/// Four claims, and they fail separately:
///
/// 1. The tail arrives, as an **exact ordered sequence** — which is what
///    catches a mark placed around the wrong call, where "the label is
///    present" would not.
/// 2. Every label on it is `Structural::CellTail`. That is the property the
///    report leans on to keep the two tables apart.
/// 3. The **request's** trace carries none of them. If a tail mark ever
///    reached the request's buffer, its spans would silently be added to the
///    request's total, which is precisely the number that has to reconcile
///    against the caller's own measurement.
/// 4. Exactly **one tail per dispatch**. Two would mean a worker left a
///    buffer behind and the next job appended to it.
#[test]
fn the_cells_tail_is_measured_on_a_trace_of_its_own() {
    let mut cfg = CellConfig::for_test("marks-tail");
    // Every dispatch, so the test is not waiting for id 128.
    cfg.stage_sample_every = 1;
    let c = cell(cfg);
    let sink = Arc::new(CollectTails::default());
    c.set_tail_traces(wisp_host::marks::CellTails {
        pool: wisp_host::marks::tail_pool_for(calibration(), c.exec_threads()),
        sink: sink.clone(),
    });
    let m = app(&c, "echo", FakeBehaviour::default());
    let p = pool();

    let (spans, _, _) = round_trip(&c, &p, &m, 0).expect("a traced reply");
    assert!(
        wait_for(|| !sink.0.lock().unwrap().is_empty(), T),
        "the cell answered a request and never recorded a tail"
    );
    // The tail runs after the reply, so the reply can arrive first; give the
    // worker a moment to finish before counting.
    std::thread::sleep(Duration::from_millis(50));
    let tails = sink.takeall();
    assert_eq!(
        tails.len(),
        1,
        "one dispatch, one tail — got {}",
        tails.len()
    );
    let tail = &tails[0];
    let got: Vec<Mark> = tail.spans.iter().map(|(l, _)| *l).collect();

    // 1. The handler tail, in order.
    assert_eq!(
        got,
        vec![
            Mark::FinishBegan,
            Mark::ActiveRemoveWanted,
            Mark::ActiveRemoveReleased,
            Mark::EndDispatchWanted,
            Mark::EndDispatchAcquired,
            Mark::EndDispatchReleased,
            Mark::TailResidentsLockWanted,
            Mark::TailResidentsLockReleased,
            Mark::ReleaseInstanceWanted,
            Mark::ReleaseInstanceAcquired,
            Mark::ReleaseInstanceReleased,
            Mark::PoolReleaseWanted,
            Mark::PoolReleaseReleased,
            Mark::FinishEnded,
            Mark::JobDone,
        ],
        "the tail is not the sequence `finish_and_release` and `exec_loop` run: {:?}",
        got.iter().map(|l| l.name()).collect::<Vec<_>>()
    );

    // 2. All of one class, which is what keeps the two tables apart.
    for l in &got {
        assert_eq!(
            l.structural(),
            wisp_host::marks::Structural::CellTail,
            "`{}` is on the tail but is not classed as tail work, so the report would print \
             it in the request's table",
            l.name()
        );
    }
    assert!(tail.span_ns > 0.0, "a tail of {} ns", tail.span_ns);
    assert!(
        !tail.truncated,
        "the tail buffer is sized at double what a tail can take"
    );
    assert_eq!(tail.raced, 0, "one thread writes the whole tail");

    // 3. And none of it is on the request.
    let req = labels(&spans);
    for l in &got {
        assert!(
            !req.contains(l),
            "`{}` reached the request's own trace, where its span would be added into the \
             request's total",
            l.name()
        );
    }
}

/// **A request that parks has no tail until it finishes.**
///
/// `finish_and_release` is the only thing that opens one, and a parking slice
/// does not go through it: it ends its slice and hands the worker back. So a
/// guest that parks once produces **two** jobs — the dispatch and the wake —
/// and exactly **one** tail, from the second. A tail per job would mean the
/// worker was measuring a release that did not happen.
#[test]
fn a_parked_slice_produces_no_tail_and_the_resumed_one_produces_exactly_one() {
    let mut cfg = CellConfig::for_test("marks-tail-park");
    cfg.stage_sample_every = 1;
    let c = cell(cfg);
    let sink = Arc::new(CollectTails::default());
    c.set_tail_traces(wisp_host::marks::CellTails {
        pool: wisp_host::marks::tail_pool_for(calibration(), c.exec_threads()),
        sink: sink.clone(),
    });
    let m = app(
        &c,
        "parker",
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", PARK_MS)],
        }]),
    );
    let p = pool();

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mk = p.checkout();
    marks::begin(&mk);
    assert!(
        c.submit_with_marks(&m, route("/x"), rec(0, "/x"), tx, mk)
            .admitted
    );
    assert!(
        wait_for(|| c.metrics().parked > 0, T),
        "the guest never parked"
    );
    // The first job is over and the worker has moved on: no tail from it.
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        sink.0.lock().unwrap().is_empty(),
        "a slice that parked produced a tail; nothing was released, so there was nothing to \
         measure"
    );

    let fid = c
        .futures()
        .live_future_ids()
        .into_iter()
        .next()
        .expect("one pending future");
    c.complete_future(fid, wisp_abi::result::ResultRecord::Ok(vec![]));
    let _ = rx.recv_timeout(T).expect("a reply after the wake");
    assert!(
        wait_for(|| !sink.0.lock().unwrap().is_empty(), T),
        "the resumed slice left no tail"
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        sink.takeall().len(),
        1,
        "two jobs ran and exactly one of them released the instance"
    );
    c.shutdown();
}

/// **A wake that runs nothing gives the parked request its trace back.**
///
/// `Cell::run_wake` reclaims the trace from `Cell::active` *before* it can know
/// whether this wake will run a slice, and there are six ways it can then
/// decide not to. Every one of them used to `return` with the trace in a local,
/// which dropped it: the request stayed parked and untraced for the rest of its
/// life, and the reply it eventually sent carried nothing. And it is not a rare
/// path — several completions for one instance produce several wakes, and the
/// first one's loop drains all of them, so every wake after the first arrives
/// to an empty table.
///
/// The observable is the **pool**, which is what makes this checkable from
/// outside the cell: a trace handed back to `Cell::active` is still checked
/// out, and a trace dropped on the floor is a buffer back in the pool. The
/// tombstone is used to force the early return because it is the one this test
/// can arrange deterministically — `Slot::tombstone` is public and the refusal
/// is the first thing `Slot::enter` checks.
#[test]
fn a_wake_that_runs_nothing_hands_the_trace_back() {
    let mut cfg = CellConfig::for_test("marks-stale-wake");
    cfg.stage_sample_every = 1;
    let c = cell(cfg);
    let m = app(
        &c,
        "parker",
        FakeBehaviour::program([Step::Park {
            imports: vec![("clock_deadline_in", PARK_MS)],
        }]),
    );
    let p = pool();
    let free = p.available();

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let mk = p.checkout();
    marks::begin(&mk);
    assert!(
        c.submit_with_marks(&m, route("/x"), rec(0, "/x"), tx, mk)
            .admitted
    );
    assert!(
        wait_for(|| c.metrics().parked > 0, T),
        "the guest never parked"
    );
    assert_eq!(
        p.available(),
        free - 1,
        "the parked request is holding its buffer, which is the premise of this test"
    );

    let id = c.resident_ids().into_iter().next().expect("one resident");
    c.slot(id).expect("the slot").tombstone();

    let fid = c
        .futures()
        .live_future_ids()
        .into_iter()
        .next()
        .expect("one pending future");
    c.complete_future(fid, wisp_abi::result::ResultRecord::Ok(vec![]));
    assert!(
        wait_for(|| c.metrics().tombstone_refusals > 0, T),
        "the wake was never refused, so the early return this test is about did not happen"
    );
    // The wake ran nothing. The request is still parked, still in `active`, and
    // must still own its trace.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        p.available(),
        free - 1,
        "the wake dropped the parked request's trace: the buffer is back in the pool while the \
         request is still in flight, so everything it does from here is unmeasured"
    );
    drop(rx);
    c.shutdown();
}
