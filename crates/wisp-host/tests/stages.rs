//! **Where a request's time goes between submit and reply.**
//!
//! The bench had three timings and everything the cell did was one opaque
//! block inside them: scheduled arrival to reply, actual release to reply, and
//! the generator's own lateness. A p99 that goes from 98 µs at eight execution
//! threads to 13,779 µs at sixty-four, with p50 and p75 unmoved and throughput
//! flat, is a queue — and nothing in the record could say *which* queue. The
//! intake lane's lock, the wait for a worker, the guest, and the reply channel
//! have four different fixes.
//!
//! Each test below is one thing that could be silently wrong about
//! [`wisp_host::cell::StageTimings`], constructed so that it *is* wrong
//! without the code that fixes it:
//!
//! | Test | What it catches |
//! |---|---|
//! | [`sampling_picks_one_request_in_n_and_zero_is_off_rather_than_a_divide_by_zero`] | a sampler that samples everything, nothing, or panics at `N = 0` |
//! | [`a_sampled_dispatch_carries_every_stage_and_an_unsampled_one_carries_none`] | the decision drifting between the four stamping points |
//! | [`the_reply_stamp_is_a_live_reading_on_the_clock_the_caller_reads`] | a stamp left at zero, on a different clock, or taken at the wrong end |
//! | [`the_queue_stage_is_the_wait_for_a_worker_and_grows_when_the_pool_is_busy`] | a `queue_ns` that is structurally zero, which is what timing the pop against the pop looks like |
//! | [`the_guest_stage_covers_every_slice_of_a_dispatch_that_parked`] | timing the first slice only, which reports a parking guest as the fastest thing in the campaign |
//! | [`the_named_stages_never_claim_more_than_the_request_took`] | a unit error, or two stages that overlap and double-count |

use std::sync::Arc;
use std::time::{Duration, Instant};

use wisp_abi::config::{AppManifest, Bindings, HandlerConfig, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_abi::result::ResultRecord;

use wisp_host::cell::{stage_sampled, Cell, CellConfig, Services, Target};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::*;
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

/// An HTTP dispatch record with a **caller-chosen** id.
///
/// `Cell::http_record` allocates from the cell's own counter, which is right
/// for production and useless here: whether a request is sampled is a function
/// of its id, so a test that cannot choose the id cannot say which results it
/// expects to carry timings.
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

#[test]
fn sampling_picks_one_request_in_n_and_zero_is_off_rather_than_a_divide_by_zero() {
    assert!(!stage_sampled(0, 0), "0 means off");
    assert!(!stage_sampled(7, 0));
    assert!(
        !stage_sampled(u64::MAX, 0),
        "and off for every id, not just small ones"
    );

    for id in 0..64u64 {
        assert!(
            stage_sampled(id, 1),
            "1 means every request, and {id} was skipped"
        );
    }

    let picked: Vec<u64> = (0..64u64).filter(|id| stage_sampled(*id, 8)).collect();
    assert_eq!(
        picked,
        vec![0, 8, 16, 24, 32, 40, 48, 56],
        "one in eight, and the same eight every time — a sampler that is not \
         reproducible cannot be checked against the count it claims"
    );
    // The rate is exact rather than approximate, which is what lets the report
    // state how many observations a percentile rests on instead of estimating
    // it. Ten thousand ids at one in 128 is exactly 79 (ids 0, 128, ..., 9984).
    assert_eq!(
        (0..10_000u64).filter(|id| stage_sampled(*id, 128)).count(),
        79
    );
}

// ---------------------------------------------------------------------------
// the decision is taken once and every stage honours it
// ---------------------------------------------------------------------------

/// **All four stages describe the same request, or none of them do.**
///
/// The failure this rules out is the one that leaves no trace in the output:
/// four stages each sampling independently would publish four percentiles over
/// four different populations. They would not be addable, the residual against
/// service time would be meaningless, and the report would look exactly the
/// same as this.
#[test]
fn a_sampled_dispatch_carries_every_stage_and_an_unsampled_one_carries_none() {
    let c = cell(CellConfig {
        stage_sample_every: 4,
        ..CellConfig::for_test("stage-pick")
    });
    let m = app(
        &c,
        "quick",
        FakeBehaviour::program([Step::Log {
            bytes: 256,
            times: 200,
        }]),
    );
    assert_eq!(
        c.stage_sample_every(),
        4,
        "the cell must publish the rate it is running at"
    );

    let mut sampled = 0usize;
    for id in 1..=20u64 {
        let out = c
            .dispatch_blocking(&m, route("/go"), rec(id, "/go"), T)
            .unwrap();
        // Any 2xx: the fake's program is one step per call into a *fresh*
        // instance, and `Reuse` hands the second dispatch the same instance
        // with the program exhausted, so the first reply is a 204 and the rest
        // are 200s. What is under test is which of them carry timings.
        assert!(
            (200..300).contains(&out.status),
            "dispatch {id} failed: {:?}",
            out.trap
        );
        match out.stages {
            Some(s) => {
                sampled += 1;
                assert_eq!(id % 4, 0, "id {id} was timed and is not a multiple of 4");
                assert!(
                    s.guest_ns > 0,
                    "id {id} was sampled but its guest stage is zero; a stage that cannot \
                     move is not a measurement"
                );
                assert!(
                    s.replied_mono_ns > 0,
                    "id {id} was sampled but its reply stamp is zero, which is the reading \
                     a struct that was never filled in gives"
                );
            }
            None => assert_ne!(id % 4, 0, "id {id} is a multiple of 4 and was not timed"),
        }
    }
    assert_eq!(sampled, 5, "one in four of twenty is five, not {sampled}");
    c.shutdown();
}

// ---------------------------------------------------------------------------
// the reply handoff
// ---------------------------------------------------------------------------

/// **The reply stamp is on the clock the caller reads, and it is taken at the
/// send.**
///
/// It is a stamp and not a duration because the interval worth having ends in
/// the *caller's* thread, and the cell is not there for it. That only works if
/// both ends read the same origin — so this brackets one dispatch between two
/// readings of [`wisp_host::mono_ns`] and requires the stamp to fall inside.
///
/// A stamp left at zero, one taken from `SystemTime`, or one taken before the
/// guest ran all fail this. A stamp taken *after* the send could not be taken
/// at all, since the result is what carries it.
#[test]
fn the_reply_stamp_is_a_live_reading_on_the_clock_the_caller_reads() {
    let c = cell(CellConfig {
        stage_sample_every: 1,
        ..CellConfig::for_test("stage-reply")
    });
    let m = app(
        &c,
        "quick",
        FakeBehaviour::program([Step::Log {
            bytes: 256,
            times: 500,
        }]),
    );

    let before = wisp_host::mono_ns();
    let out = c
        .dispatch_blocking(&m, route("/go"), rec(1, "/go"), T)
        .unwrap();
    let after = wisp_host::mono_ns();
    let s = out.stages.expect("every request is sampled at 1");

    assert!(
        s.replied_mono_ns >= before && s.replied_mono_ns <= after,
        "the reply was stamped at {} ns, outside the [{before}, {after}] ns bracket the \
         caller observed around the whole dispatch — the two ends are not reading the same \
         clock, and the handoff time the harness computes from this would be nonsense",
        s.replied_mono_ns
    );
    // The guest ran before the reply was stamped, so the stamp cannot be
    // earlier than the start of the call plus the guest's own time. This is
    // what separates "stamped at the send" from "stamped at admission".
    assert!(
        s.replied_mono_ns.saturating_sub(before) >= s.guest_ns,
        "the reply was stamped {} ns into the call but the guest alone ran for {} ns, so \
         the stamp was taken before the work it is supposed to follow",
        s.replied_mono_ns.saturating_sub(before),
        s.guest_ns
    );
    c.shutdown();
}

// ---------------------------------------------------------------------------
// the queue stage
// ---------------------------------------------------------------------------

/// **`queue_ns` is the wait for a worker, and it moves when there is one.**
///
/// The failure mode here is a stage that is structurally zero: stamp the pop
/// against the pop and every reading is a few nanoseconds, on a cell whose
/// whole problem is that requests are queueing. It would look healthy in every
/// run and would be the reason nobody found the queue.
///
/// So: one execution thread, and two requests. The first occupies the thread
/// for a long time; the second cannot start until it finishes, and its wait is
/// the first one's guest time. A run with the stage measured correctly puts
/// the second's `queue_ns` above the first's `guest_ns`; a run with it
/// measured at the wrong end puts it near zero.
#[test]
fn the_queue_stage_is_the_wait_for_a_worker_and_grows_when_the_pool_is_busy() {
    let c = cell(CellConfig {
        exec_threads: 1,
        max_hot: 1,
        stage_sample_every: 1,
        ..CellConfig::for_test("stage-queue")
    });
    // Enough log lines that the first dispatch holds the single thread for
    // long enough to be unambiguous against scheduler noise, and few enough
    // that the test is not slow.
    let m = app(
        &c,
        "slow",
        FakeBehaviour::program([Step::Log {
            bytes: 4096,
            times: 20_000,
        }]),
    );

    let first = c.submit(&m, route("/go"), rec(1, "/go")).expect("admitted");
    // The second must be *behind* the first in the lane, not racing it into an
    // empty queue: without this the test passes or fails on thread start-up
    // order, which is the kind of flake that gets a real assertion deleted.
    assert!(
        wait_for(|| c.hot_count() > 0, Duration::from_secs(10)),
        "the first dispatch never reached the guest, so nothing was occupying the pool"
    );
    let second = c.submit(&m, route("/go"), rec(2, "/go")).expect("admitted");

    let a = first
        .recv_timeout(T)
        .expect("first never completed")
        .stages
        .expect("sampled");
    let b = second
        .recv_timeout(T)
        .expect("second never completed")
        .stages
        .expect("sampled");

    assert!(
        b.queue_ns >= a.guest_ns / 2,
        "the second request waited {} ns for the one execution thread while the first spent \
         {} ns inside the guest; a queue stage that cannot see that is measuring the pop \
         against itself",
        b.queue_ns,
        a.guest_ns
    );
    assert!(
        b.queue_ns > a.queue_ns,
        "the request that had to wait ({} ns) must record more queueing than the one that \
         did not ({} ns)",
        b.queue_ns,
        a.queue_ns
    );
    c.shutdown();
}

#[test]
fn the_guest_stage_covers_every_slice_of_a_dispatch_that_parked() {
    let c = cell(CellConfig {
        stage_sample_every: 1,
        ..CellConfig::for_test("stage-slices")
    });
    // Park with a timer that will not fire, then — on the wake — do a
    // measurable amount of work. If only the first slice were timed, the
    // second slice's work would be invisible.
    let m = app(
        &c,
        "parker",
        FakeBehaviour::program([
            Step::Park {
                imports: vec![("clock_deadline_in", PARK_MS)],
            },
            Step::Log {
                bytes: 4096,
                times: 20_000,
            },
        ]),
    );

    let before: Vec<u64> = c.futures().live_future_ids();
    let rx = c
        .submit(&m, route("/wake"), rec(1, "/wake"))
        .expect("admitted");
    let mut id = None;
    assert!(
        wait_for(
            || {
                id = c
                    .futures()
                    .live_future_ids()
                    .into_iter()
                    .find(|i| !before.contains(i));
                id.is_some()
            },
            Duration::from_secs(10)
        ),
        "the dispatch never parked, so there is no second slice to account for"
    );

    // The park itself is wall time the guest is *not* running: it returned,
    // and invariant 3 means it holds no thread at all. Sleeping here makes the
    // two candidate implementations disagree by more than any noise — a guest
    // stage measured from first entry to final reply would swallow this, and a
    // sum over slices cannot.
    std::thread::sleep(Duration::from_millis(300));
    c.complete_future(id.unwrap(), ResultRecord::Ok(b"v".to_vec()));

    let out = rx.recv_timeout(T).expect("never woke");
    assert_eq!(out.slices, 2, "expected the dispatch and one wake");
    let s = out.stages.expect("sampled");
    assert_eq!(
        s.guest_ns, out.wall_ns,
        "the guest stage is the per-slice sum `settle` keeps"
    );
    assert!(
        s.guest_ns > 0,
        "two slices ran and the guest stage is zero, so the wake's work was never added"
    );
    assert!(
        s.guest_ns < 300_000_000,
        "the guest stage is {} ns, which is more than the 300 ms this request spent parked — \
         park time is not guest time, and charging it here would make every awaiting \
         workload look CPU-bound",
        s.guest_ns
    );
    c.shutdown();
}

// ---------------------------------------------------------------------------
// the stages against the whole
// ---------------------------------------------------------------------------

/// **The named stages never claim more time than the request took.**
///
/// The decomposition is only worth publishing if it is bounded by the thing it
/// decomposes. A unit error — microseconds recorded into a nanosecond field —
/// or two stages that overlap both show up here and nowhere else, because a
/// stage table on its own has nothing to be wrong against.
#[test]
fn the_named_stages_never_claim_more_than_the_request_took() {
    let c = cell(CellConfig {
        stage_sample_every: 1,
        ..CellConfig::for_test("stage-sum")
    });
    let m = app(
        &c,
        "quick",
        FakeBehaviour::program([Step::Log {
            bytes: 512,
            times: 2_000,
        }]),
    );

    for id in 1..=5u64 {
        let t0 = Instant::now();
        let out = c
            .dispatch_blocking(&m, route("/go"), rec(id, "/go"), T)
            .unwrap();
        let observed = t0.elapsed().as_nanos() as u64;
        let s = out.stages.expect("sampled");
        let named = s.submit_ns + s.queue_ns + s.guest_ns;
        assert!(
            named <= observed,
            "the three closed stages sum to {named} ns (submit {}, queue {}, guest {}) on a \
             request the caller watched take {observed} ns",
            s.submit_ns,
            s.queue_ns,
            s.guest_ns,
        );
        assert!(
            s.guest_ns > 0 && s.guest_ns <= observed,
            "the guest stage is {} ns against an observed {observed} ns",
            s.guest_ns
        );
    }
    c.shutdown();
}
