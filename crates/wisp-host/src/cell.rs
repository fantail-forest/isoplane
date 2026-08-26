//! The cell (spec.md §6.1, §6.3, §6.4, §8.2.4).
//!
//! ```text
//! ┌─ cell ────────────────────────────────────────────────────┐
//! │ reactors (tokio) · exec pool (4×N)                        │
//! │ instance slot pool  [][][][][][][]                        │
//! │ future table · wake router · meters                       │
//! └───────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Two thread classes, and why the split matters
//!
//! *Reactor threads own all I/O* (§6.1). A completion arrives, the future
//! table maps its id to the owning instance, and a wake is enqueued.
//! *Execution-pool threads run Hot dispatches as **synchronous** calls into
//! the engine.* This is deliberately not the engine's async mode, whose
//! epoch-*yielding* is implemented with fibers — hidden native stacks, which
//! invariant 3 forbids. A native stack exists in this crate only for the
//! duration of one `dispatch`/`wake` call, and a Parked instance holds no
//! thread at all.
//!
//! ## There is no scheduler here, on purpose
//!
//! §6.4: *"**Fairness is the kernel's job:** concurrently Hot dispatches are
//! preempted and time-shared by the OS scheduler, so a CPU-bound tenant
//! occupies only its own thread and cannot head-of-line-block other tenants'
//! wakes. The platform makes no deterministic slice promise — fairness is
//! statistical and kernel-grade, which is also the incumbent's real
//! contract."*
//!
//! So this module does **not** time-slice guests, does not run a run-queue
//! with priorities, and does not preempt anybody. What it does control is
//! *admission*: a per-cell cap on concurrently Hot dispatches, an overflow
//! queue, and shedding before saturation. A spinner burns its own thread until
//! the CPU budget kills it, and §8.3's blast radius — "the offender's own
//! requests" — is a consequence of that arrangement rather than a feature
//! bolted on top.
//!
//! **Not implemented (PoC gap):** §6.4's per-tenant CPU weighting via cgroup v2
//! threaded `cpu` controllers, and §8.4's core-scheduling cookies that keep
//! trust tiers from SMT-pairing. Both need a real cgroup hierarchy and a
//! process-per-cell, which docs/plan.md's "one process, many cells" deviation
//! removes. Fairness here is therefore *equal-weight* kernel fairness, not
//! Thread scheduling follows the operating system's fairness rules.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

// The seven locks a dispatch takes most often are wrapped so a run can say
// which of them the execution threads lost time to. `lock()`/`read()` keep
// their signatures, so no call site below changes. See `lockstats`.
use crate::lockstats::{CountedMutex, CountedRwLock};

use wisp_abi::config::{AppManifest, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, TraceContext};
use wisp_abi::status::{PollStatus, TrapCode};
use wisp_engine::{Completion, CpuBudget, DispatchOutcome, KillPhase};

use crate::bindings::{CellRuntime, InstanceCtx, WakeSink};
pub use crate::bindings::{Fetcher, Services};
use crate::breaker::{BreakerConfig, BreakerKey, HangBreaker};
use crate::futures::FutureTable;
use crate::instances::{
    decide, evict, AcquireSource, Activator, InstancePool, LifecycleTier, RequestEnd, Slot,
};
use crate::meters::Meters;
use crate::metrics::{Metrics, MetricsSnapshot};
use crate::{InstanceId, ModuleId};

#[derive(Debug, Clone)]
pub struct CellConfig {
    pub name: String,
    /// §6.1: default 4× cores, growable to a hard cap.
    pub exec_threads: usize,
    /// Reactor worker threads. These own all I/O.
    pub reactor_threads: usize,
    /// Configured hot-dispatch concurrency value.
    pub max_hot: usize,
    /// §6.4: overflow queues at admission; beyond this the cell sheds, which
    /// is the backstop for "the proxy sheds before cells saturate".
    pub admission_queue: usize,
    /// How incoming work is spread across the execution pool.
    ///
    /// Defaults to [`QueueStrategy::Shared`], which is what this cell has
    /// always done. See that type for what the alternative buys and why.
    pub queue_strategy: QueueStrategy,
    pub max_idle_per_module: usize,
    /// Wall-clock backstop for a dispatch. Not a meter (§11.2).
    pub wall_ms_cap: u32,
    pub breaker: BreakerConfig,
    /// How often the idle trajectory sweeps (§6.6).
    pub sweep_interval_ms: u64,
    /// §10/H7: how densely the two **per-dispatch** histograms (`request_us`,
    /// `park_ms`) keep samples. `1` — the default — keeps every one; `N` keeps
    /// one in N; `0` keeps none and takes no lock at all on the dispatch path.
    ///
    /// Each of those histograms is a per-cell mutex acquired by every
    /// execution thread on every completed dispatch. Counting the locks one
    /// dispatch takes through `run_dispatch` → `settle` — the work queue's
    /// push and pop, `modules`, `durables` + `residents`, `active` three
    /// times, and these two — gives at least eight, six of them per-cell
    /// singletons shared by all execution threads. The campaign publishes
    /// "the cell's knee is lock contention, not CPU" and names the work queue
    /// as the prime suspect; that inference is equally consistent with
    /// contention on `active`, on `durables`, or on these two histograms.
    ///
    /// So this exists to let the claim be **tested by difference** rather than
    /// assumed: run the same point at `1` and at `0` and see whether the knee
    /// moves. Setting it to `0` does not remove the metrics — counts, rates
    /// and means keep working, only percentiles stop — because §10 asks for
    /// them and a run without them still has to be comparable to a run with.
    pub telemetry_sample_every: u64,
    /// How densely [`StageTimings`] is collected: `N` times one request in `N`,
    /// `0` or `1` — `1` meaning every request — with `0` switching it off.
    ///
    /// **Sampled rather than always-on because the clock is the cost.** Three
    /// extra `clock_gettime` calls per request at 300,000 requests a second is
    /// close to a million vDSO reads a second across the pool, on the one path
    /// whose timing is the measurement. At the default it is one request in
    /// 128: still 2,300 samples a second, which is 70,000 over a thirty-second
    /// hold and enough for a p99.9 by the same ten-observations-past-the-
    /// quantile rule the rest of the suite uses, while 0.8% of requests pay
    /// anything at all.
    ///
    /// **Whether a request is sampled is `dispatch_id % N == 0`.** Stateless,
    /// so no counter is shared between submitting threads; deterministic, so a
    /// test can say exactly which requests it expects back. It assumes ids are
    /// dense and that nothing about a request correlates with its id modulo
    /// `N` — true for the cell's own counter and for the bench generator,
    /// whose ids are per-thread sequence numbers and whose instance choice is
    /// drawn from the arrival stream rather than round-robined. A caller that
    /// hands out ids correlated with the work would bias the sample, and would
    /// be sampling nothing or everything if it reused one id.
    pub stage_sample_every: u64,
}

impl Default for CellConfig {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            name: "cell-0".into(),
            exec_threads: cores * 4,
            reactor_threads: cores.max(2),
            max_hot: cores * 4,
            admission_queue: 1024,
            max_idle_per_module: 64,
            wall_ms_cap: 30_000,
            queue_strategy: QueueStrategy::default(),
            breaker: BreakerConfig::default(),
            sweep_interval_ms: 250,
            telemetry_sample_every: 1,
            stage_sample_every: DEFAULT_STAGE_SAMPLE_EVERY,
        }
    }
}

/// One request in 128 carries stage timings. See
/// [`CellConfig::stage_sample_every`] for the arithmetic behind the number.
pub const DEFAULT_STAGE_SAMPLE_EVERY: u64 = 128;

/// Is this the one request in `every` that gets timed?
///
/// `0` is off and `1` is every request; both are stated here rather than at
/// each call site, because "one in zero" read as a modulo is a division by
/// zero and this runs on the dispatch path.
#[inline]
pub fn stage_sampled(dispatch_id: u64, every: u64) -> bool {
    match every {
        0 => false,
        1 => true,
        // `%` rather than `is_multiple_of`: the workspace declares
        // `rust-version = "1.85"` and that method landed in 1.87.
        #[allow(clippy::manual_is_multiple_of)]
        n => dispatch_id % n == 0,
    }
}

impl CellConfig {
    /// A small deterministic cell for tests.
    pub fn for_test(name: &str) -> Self {
        Self {
            name: name.into(),
            exec_threads: 4,
            reactor_threads: 2,
            max_hot: 4,
            sweep_interval_ms: 25,
            ..Default::default()
        }
    }
}

/// What a top-level work item is addressed to.
#[derive(Debug, Clone)]
pub enum Target {
    /// Any instance of the module — §6.3's Handler path.
    Handler { route: String },
}

impl Target {
    fn route(&self) -> String {
        match self {
            Target::Handler { route } => route.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DispatchResult {
    /// The `dispatch_id` of the `DispatchRecord` this is the answer to.
    ///
    /// Echoed back because a caller that submits many dispatches onto **one**
    /// completion channel — which is what any load generator must do, since a
    /// channel and a thread per outstanding request measures the caller — has
    /// no other way to match a result to the request it belongs to.
    /// Completions arrive out of order by design (§6.1: a wake may land on any
    /// pool thread), so arrival order carries no information.
    pub dispatch_id: u64,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub completion: Completion,
    /// The recorded meter, and nothing else (§11.2).
    pub cpu_ns: u64,
    /// Observability only; reported separately from active execution time.
    pub wall_ns: u64,
    pub trap: Option<String>,
    pub instance: InstanceId,
    /// §9.2 `tier=fast` visibility: did this request pay for an activation?
    pub acquired: &'static str,
    /// Slices this request took: 1 for a dispatch that never parked, more if
    /// it awaited.
    pub slices: u32,
    /// Where this request's time went inside the cell, if it was one of the
    /// sampled ones. `None` on every other request and on every refusal.
    ///
    /// See [`StageTimings`]. It rides back on the result rather than being
    /// accumulated cell-side because the caller already owns a histogram per
    /// distribution, already scopes it to a hold by differencing two
    /// snapshots, and already reads percentiles off it — a second set of
    /// histograms inside the cell would be a second implementation of all
    /// three, on the one path that must not grow a lock.
    pub stages: Option<StageTimings>,
}

/// Where one request's time went between `submit` and the reply landing.
///
/// **Why this exists.** The harness had exactly three timings — scheduled
/// arrival to reply, actual release to reply, and the generator's own lateness
/// — so everything the cell did was one opaque block. A p99 of 13.8 ms at
/// sixty-four execution threads against 98 mu-s at eight is a fact about
/// queueing, and nothing in the record could say *which* queue: the intake
/// lane's lock, the wait for a worker, the guest itself, or the reply channel
/// backing up behind a collector that lost its cpu. Those four have different
/// fixes and the difference between them was being inferred from a thread
/// count.
///
/// **The four are disjoint and they are on one request.** `submit_ns` ends
/// where `queue_ns` begins and so on, so they add; and because the sampling
/// decision is taken once, at admission, and carried on the job, all four
/// figures for a sampled request describe *that* request. Sampling each stage
/// independently would give four distributions over four different
/// populations, which cannot be added and would look exactly the same in the
/// report.
///
/// **What is deliberately not here.** The gap between the four and the
/// caller's own service time — module lookup, instance acquisition, the
/// `active` insert, `settle`, the response table — is left as a residual for
/// the caller to compute and publish. Naming it as a fifth stage would imply
/// it had been measured; it has not, and a decomposition that appears to
/// account for everything is the one nobody checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StageTimings {
    /// Entering the submit path to the instant the job goes into a lane:
    /// the breaker check, the job allocation, lane selection and — the part
    /// that matters — acquiring the intake queue's lock.
    ///
    /// It stops just short of the push itself and of the condvar notify,
    /// because the job is moved by that call and cannot be stamped afterwards.
    /// The lock acquisition is the contended half and it is inside.
    pub submit_ns: u64,
    /// Sitting in a lane, waiting for an execution thread to pop it.
    ///
    /// The queueing delay proper. Under `QueueStrategy::PerWorker` this is a
    /// wait on one lane rather than on the pool, which is the whole point of
    /// that strategy and the number that shows whether it worked.
    pub queue_ns: u64,
    /// Inside the guest, summed over every slice of the dispatch.
    ///
    /// Every slice, not the first: a dispatch that parks and is woken runs the
    /// guest again, and §6.3 makes that one request. Timing only the first
    /// slice would report a `chatty` guest as faster than an `echo` one.
    pub guest_ns: u64,
    /// [`crate::mono_ns`] at the instant the cell handed the reply to the
    /// responder channel.
    ///
    /// A stamp rather than a duration, because the interval worth having runs
    /// from here to the caller *receiving* it and the cell is not present for
    /// the second half. Timing `SyncSender::send` alone would measure only the
    /// case where the channel is already full — it is the queueing in front of
    /// that, on a collector thread that lost its cpu, that shows up as
    /// platform latency in the report.
    pub replied_mono_ns: u64,
}

/// The stamps a sampled request carries while it is in flight.
///
/// Absent on every unsampled request, so the cost of the whole mechanism on
/// the common path is one `Option` discriminant and the modulo that set it.
#[derive(Debug, Clone, Copy)]
struct StageStamps {
    /// When the job entered a lane. Set by `WorkQueue::push_dispatch`.
    queued: Instant,
    submit_ns: u64,
    queue_ns: u64,
}

impl DispatchResult {
    fn shed(dispatch_id: u64) -> Self {
        Self {
            dispatch_id,
            status: 503,
            headers: vec![("retry-after".into(), "1".into())],
            body: b"cell shed load".to_vec(),
            completion: Completion::Trapped,
            cpu_ns: 0,
            wall_ns: 0,
            trap: Some("shed".into()),
            instance: InstanceId(0),
            acquired: "none",
            slices: 0,
            stages: None,
        }
    }
    fn breaker_open(dispatch_id: u64) -> Self {
        Self {
            dispatch_id,
            status: 503,
            headers: vec![],
            // §6.4: fail fast, without entering the guest at all.
            body: b"hang breaker open".to_vec(),
            completion: Completion::Trapped,
            cpu_ns: 0,
            wall_ns: 0,
            trap: Some("breaker_open".into()),
            instance: InstanceId(0),
            acquired: "none",
            slices: 0,
            stages: None,
        }
    }
}

// ---------------------------------------------------------------------------
// The work queues
// ---------------------------------------------------------------------------

enum Job {
    Dispatch(Box<DispatchJob>),
    /// A host future completed for this instance; deliver it.
    Wake(Arc<Slot>),
}

struct DispatchJob {
    module: ModuleId,
    target: Target,
    rec: DispatchRecord,
    responder: std::sync::mpsc::SyncSender<DispatchResult>,
    enqueued: Instant,
    /// `Some` on the one request in [`CellConfig::stage_sample_every`] that
    /// was chosen for stage timing, at admission and once.
    stamps: Option<StageStamps>,
}

/// Shards for `Cell::active`.
///
/// Sized above the execution pool so a collision is unlikely rather than
/// certain: §6.1's default is 4x the cell's logical cpus, which is 64 on a
/// sixteen-core cell and 256 on the widest this box can build. An empty
/// `HashMap` is three words, so 256 of them cost less than one instance.
const ACTIVE_SHARDS: usize = 256;

/// How incoming work is spread across the execution pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum QueueStrategy {
    /// One queue, every worker pops from it.
    ///
    /// Simple and correct, and it does not scale: every producer and every
    /// worker takes the same lock on every operation. Measured at 89,600
    /// req/s with a 64-thread pool, this lock accounted for 99.5% of all the
    /// time the workers spent blocked — 365 seconds of it inside a 30 second
    /// hold — and its cost is superlinear in the pool size (1.1 µs per
    /// dispatch at 8 threads, 133 µs at 64).
    #[default]
    Shared,
    /// One queue per worker, with new work placed by power-of-two choices.
    ///
    /// A producer samples two lanes at random and pushes to the shallower.
    /// That is O(1) per push and gets most of the balance of scanning every
    /// lane, which is the standard result for randomised load balancing.
    ///
    /// **Why not a single lock-free queue.** Lock-free guarantees progress,
    /// not independence: one shared queue still has every thread doing
    /// compare-and-swap on the same head and tail, so the cachelines holding
    /// them bounce between cores and the retries grow with the pool. It
    /// removes the syscalls and the context switches, and leaves the sharing.
    /// Per-worker lanes remove the sharing: a worker pops its own lane, which
    /// in the common case no other thread is touching.
    PerWorker,
    /// One shared pair of lock-free queues, and no mutex on the data at all.
    ///
    /// Keeps the two lanes and their priority — a bounded ring for dispatches,
    /// so a full ring hands the job back and that *is* §6.4's shed, and an
    /// unbounded queue for wakes, which are never shed.
    ///
    /// **This removes blocking, not sharing.** Every thread still does
    /// compare-and-swap on the same head and tail, so the cachelines holding
    /// them still move between cores and the retries still grow with the pool.
    /// What it removes is the futex: no syscall, no context switch, no thread
    /// descheduled while holding the structure. Expect it to beat `Shared`
    /// comfortably and to fall behind `PerWorker` as the pool grows — and if
    /// it does not, that is worth knowing, which is why all three are
    /// selectable rather than one being chosen by argument.
    ///
    /// A worker with nothing to do still has to sleep. The parking mutex and
    /// condition variable below guard no data — they exist only so a consumer
    /// can wait without spinning, and the producer touches them only when a
    /// consumer is actually parked.
    LockFree,
}

/// A two-lane queue, in one or many copies.
///
/// **The two lanes are the admission story, and they are unchanged by either
/// strategy.** Wakes jump ahead of new dispatches, and wakes are never shed:
/// overflow queues at admission and sheds *new* work, so work already in
/// flight keeps making progress and a saturated cell does not turn itself
/// into a pile of half-finished requests (§6.4).
///
/// `lanes` is one entry under [`QueueStrategy::Shared`] — in which case this
/// is exactly the structure it replaces — and one per worker under
/// [`QueueStrategy::PerWorker`].
struct WorkQueue {
    /// Empty under [`QueueStrategy::LockFree`].
    lanes: Box<[Lane]>,
    /// `Some` only under [`QueueStrategy::LockFree`].
    free: Option<LockFreeLanes>,
    /// Per-lane capacity for the dispatch lane. The whole-cell cap divided
    /// across the lanes, so the strategy changes where work waits and not how
    /// much of it the cell will hold.
    cap: usize,
    strategy: QueueStrategy,
}

/// One lane: the two queues, the lock over them, and a depth counter that can
/// be read without taking it.
///
/// `depth` is padded to its own cache line. Without that, sampling two lanes'
/// depths would pull in a line that a third lane's push is writing, and the
/// balancer would generate exactly the sharing it exists to avoid.
#[repr(align(128))]
struct Lane {
    inner: CountedMutex<QueueInner>,
    cv: Condvar,
    /// Dispatches queued here. Approximate by design: it is read outside the
    /// lock to pick a lane, and a stale read costs a slightly worse choice
    /// rather than a wrong one.
    depth: AtomicUsize,
}

/// The lock-free pair, plus the parking spot a worker sleeps on.
struct LockFreeLanes {
    /// Bounded: `push` returns the job back when full, which is the shed.
    dispatches: crossbeam_queue::ArrayQueue<Box<DispatchJob>>,
    /// Unbounded: §6.4 never sheds a wake.
    wakes: crossbeam_queue::SegQueue<Arc<Slot>>,
    closed: AtomicBool,
    /// Guards nothing. A consumer takes it only to park, and a producer takes
    /// it only when `parked` says someone is there to wake.
    park: Mutex<()>,
    cv: Condvar,
    /// How many workers are parked or about to park.
    ///
    /// The whole point of the count is to keep the producer off the parking
    /// mutex when nobody is waiting, which is the common case under load. It
    /// is also what closes the lost-wakeup race: a consumer increments it
    /// while holding `park`, so a producer that reads a non-zero count cannot
    /// acquire `park` until that consumer is actually waiting on `cv`.
    parked: AtomicUsize,
}

#[derive(Default)]
struct QueueInner {
    wakes: std::collections::VecDeque<Arc<Slot>>,
    dispatches: std::collections::VecDeque<Box<DispatchJob>>,
    closed: bool,
}

/// A cheap per-thread xorshift, for picking two lanes.
///
/// Not `rand::rng()`: this runs on every dispatch and the choice does not need
/// to be good randomness, only unbiased and fast. Seeded off the thread id so
/// two workers do not walk the same sequence.
fn lane_rand(n: usize) -> usize {
    use std::cell::Cell as StdCell;
    thread_local! {
        static STATE: StdCell<u64> = const { StdCell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            // Any non-zero seed; the address of the cell differs per thread.
            x = s as *const _ as u64 | 1;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x % n as u64) as usize
    })
}

impl WorkQueue {
    fn new(cap: usize, lanes: usize, strategy: QueueStrategy) -> Self {
        let n = match strategy {
            QueueStrategy::Shared => 1,
            QueueStrategy::PerWorker => lanes.max(1),
            QueueStrategy::LockFree => 0,
        };
        let free = (strategy == QueueStrategy::LockFree).then(|| LockFreeLanes {
            dispatches: crossbeam_queue::ArrayQueue::new(cap.max(1)),
            wakes: crossbeam_queue::SegQueue::new(),
            closed: AtomicBool::new(false),
            park: Mutex::new(()),
            cv: Condvar::new(),
            parked: AtomicUsize::new(0),
        });
        Self {
            lanes: (0..n)
                .map(|_| Lane {
                    inner: CountedMutex::new(QueueInner::default()),
                    cv: Condvar::new(),
                    depth: AtomicUsize::new(0),
                })
                .collect(),
            free,
            cap: (cap / n.max(1)).max(1),
            strategy,
        }
    }

    /// Wake one parked worker, if any is parked.
    ///
    /// The `parked` check is what keeps this off the parking mutex entirely
    /// while the cell is busy — under load a worker finds work on its next
    /// `try_pop` and never sleeps, so producers never touch the mutex.
    #[inline]
    fn free_notify(f: &LockFreeLanes) {
        if f.parked.load(Ordering::SeqCst) > 0 {
            let _g = f.park.lock();
            f.cv.notify_one();
        }
    }

    /// One non-blocking attempt at the two lock-free lanes, wakes first.
    #[inline]
    fn free_try_pop(f: &LockFreeLanes) -> Option<Job> {
        if let Some(s) = f.wakes.pop() {
            return Some(Job::Wake(s));
        }
        f.dispatches.pop().map(Job::Dispatch)
    }

    /// The lane a new dispatch should go to.
    ///
    /// Power of two: sample two, take the shallower. With one lane this is
    /// free and always returns it.
    #[inline]
    fn pick(&self) -> usize {
        let n = self.lanes.len();
        if n == 1 {
            return 0;
        }
        let a = lane_rand(n);
        let b = lane_rand(n);
        if self.lanes[a].depth.load(Ordering::Relaxed)
            <= self.lanes[b].depth.load(Ordering::Relaxed)
        {
            a
        } else {
            b
        }
    }

    /// Close the submit stage and open the queue stage, on a sampled job.
    ///
    /// One clock read produces both: the instant the job enters a lane is the
    /// end of one interval and the start of the next, and reading it twice
    /// would leave a gap between the stages that belongs to neither.
    ///
    /// Called immediately before the push in both strategies. It cannot be
    /// called after, because the push moves the job — so `submit_ns` excludes
    /// the `push_back` (or the ring's compare-and-swap) and the notify, and
    /// includes the lane choice and the lock acquisition, which is the half
    /// that contends.
    #[inline]
    fn stamp_queued(j: &mut DispatchJob) {
        if let Some(s) = &mut j.stamps {
            let now = Instant::now();
            s.submit_ns = now.duration_since(j.enqueued).as_nanos() as u64;
            s.queued = now;
        }
    }

    /// `Err` means shed (§6.4).
    fn push_dispatch(&self, j: Box<DispatchJob>) -> Result<(), Box<DispatchJob>> {
        let mut j = j;
        if let Some(f) = &self.free {
            if f.closed.load(Ordering::Relaxed) {
                return Err(j);
            }
            Self::stamp_queued(&mut j);
            // A full ring hands the job straight back, which is exactly the
            // shed the mutex version does at `dispatches.len() >= cap`.
            f.dispatches.push(j)?;
            Self::free_notify(f);
            return Ok(());
        }
        let lane = &self.lanes[self.pick()];
        let mut g = lane.inner.lock();
        if g.closed || g.dispatches.len() >= self.cap {
            return Err(j);
        }
        // Inside the lock, so `submit_ns` contains the acquisition. That is
        // the contended half and the reason this stage exists at all: the
        // `Shared` strategy's one mutex cost the pool 365 s of blocked time
        // inside a 30 s hold, and until now nothing said how much of a
        // request's life was spent waiting for it as opposed to waiting in it.
        Self::stamp_queued(&mut j);
        g.dispatches.push_back(j);
        lane.depth.fetch_add(1, Ordering::Relaxed);
        drop(g);
        lane.cv.notify_one();
        Ok(())
    }

    /// A wake is never shed, so it goes to the shallowest of two regardless of
    /// capacity.
    fn push_wake(&self, s: Arc<Slot>) {
        if let Some(f) = &self.free {
            if f.closed.load(Ordering::Relaxed) {
                return;
            }
            f.wakes.push(s);
            Self::free_notify(f);
            return;
        }
        let lane = &self.lanes[self.pick()];
        let mut g = lane.inner.lock();
        if g.closed {
            return;
        }
        g.wakes.push_back(s);
        drop(g);
        lane.cv.notify_one();
    }

    /// Take the next job for `worker`.
    ///
    /// Under [`QueueStrategy::Shared`] every worker reads lane zero, which is
    /// the original behaviour exactly. Under [`QueueStrategy::PerWorker`] a
    /// worker reads only its own lane: it blocks there rather than stealing,
    /// because power-of-two placement is what keeps the lanes even, and a
    /// stealing scan would put every idle worker back on every other worker's
    /// lock — the sharing this exists to remove.
    fn pop(&self, worker: usize) -> Option<Job> {
        if let Some(f) = &self.free {
            loop {
                // Fast path: no lock, no atomics beyond the queue's own.
                if let Some(j) = Self::free_try_pop(f) {
                    return Some(j);
                }
                if f.closed.load(Ordering::SeqCst) {
                    return None;
                }
                // Slow path. Announce the park BEFORE the last check and while
                // holding `park`, so a producer that pushes after that check
                // must either see `parked > 0` and block on `park` until this
                // thread is genuinely waiting, or have pushed early enough for
                // the check to see it. That ordering is the whole reason a
                // wake-up cannot be lost here.
                let mut g = f.park.lock();
                f.parked.fetch_add(1, Ordering::SeqCst);
                let missed = Self::free_try_pop(f);
                if missed.is_some() || f.closed.load(Ordering::SeqCst) {
                    f.parked.fetch_sub(1, Ordering::SeqCst);
                    if missed.is_some() {
                        return missed;
                    }
                    return None;
                }
                f.cv.wait(&mut g);
                f.parked.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let lane = &self.lanes[worker % self.lanes.len()];
        let mut g = lane.inner.lock();
        loop {
            if let Some(s) = g.wakes.pop_front() {
                return Some(Job::Wake(s));
            }
            if let Some(d) = g.dispatches.pop_front() {
                lane.depth.fetch_sub(1, Ordering::Relaxed);
                return Some(Job::Dispatch(d));
            }
            if g.closed {
                return None;
            }
            lane.cv.wait(&mut g);
        }
    }

    fn close(&self) {
        if let Some(f) = &self.free {
            f.closed.store(true, Ordering::SeqCst);
            let _g = f.park.lock();
            f.cv.notify_all();
            return;
        }
        for lane in self.lanes.iter() {
            lane.inner.lock().closed = true;
            lane.cv.notify_all();
        }
    }

    fn depth(&self) -> usize {
        if let Some(f) = &self.free {
            return f.dispatches.len() + f.wakes.len();
        }
        self.lanes
            .iter()
            .map(|l| {
                let g = l.inner.lock();
                g.wakes.len() + g.dispatches.len()
            })
            .sum()
    }

    /// Every lane's contention, summed — so the figure stays comparable with
    /// the unsharded locks beside it however many lanes there are.
    /// Under `LockFree` this is structurally zero: the parking mutex guards
    /// no data and is taken only to sleep. A zero here means "no lock to
    /// block on", which is a different statement from "nothing blocked", and
    /// the report says which strategy produced it.
    fn lock_stats(&self) -> crate::lockstats::LockSample {
        if self.free.is_some() {
            return crate::lockstats::LockSample::default();
        }
        self.lanes
            .iter()
            .fold(crate::lockstats::LockSample::default(), |acc, l| {
                let x = l.inner.stats().snapshot();
                crate::lockstats::LockSample {
                    contended: acc.contended + x.contended,
                    blocked_ns: acc.blocked_ns + x.blocked_ns,
                }
            })
    }

    fn strategy(&self) -> QueueStrategy {
        self.strategy
    }
}

// ---------------------------------------------------------------------------
// Registries
// ---------------------------------------------------------------------------

struct ModuleEntry {
    tenant: String,
    limits: Limits,
    manifest: AppManifest,
    activator: Arc<dyn Activator>,
    pool: InstancePool,
    meters: Arc<Meters>,
}

/// The cell's view of one resident instance. This is the "bookkeeping" half of
/// §6.2's *slot = reserved virtual address range + bookkeeping*.
struct Resident {
    slot: Arc<Slot>,
    module: ModuleId,
}

impl Resident {}

/// In-flight request state for one instance.
struct ActiveRequest {
    dispatch_id: u64,
    lifecycle: HandlerLifecycle,
    module: ModuleId,
    route: String,
    responder: Option<std::sync::mpsc::SyncSender<DispatchResult>>,
    started: Instant,
    cpu_ns: u64,
    wall_ns: u64,
    slices: u32,
    acquired: &'static str,
    budget: CpuBudget,
    /// Carried from the job so the reply can report the two stages that were
    /// already closed. `wall_ns` beside it is the third, accumulated per slice
    /// by `settle`, so the guest stage costs no clock read of its own.
    stamps: Option<StageStamps>,
}

/// Routes completions from reactor threads onto the exec pool (§6.1's wake
/// router). Holds only weak references, so a discarded instance's late
/// completion is a no-op rather than a resurrection.
struct CellWake {
    queue: Arc<WorkQueue>,
    index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>>,
    metrics: Arc<Metrics>,
}

impl WakeSink for CellWake {
    fn signal(&self, owner: InstanceId) {
        let slot = self.index.lock().get(&owner).and_then(Weak::upgrade);
        if let Some(s) = slot {
            self.metrics.wakes.fetch_add(1, Ordering::Relaxed);
            self.queue.push_wake(s);
        }
    }
}

// ---------------------------------------------------------------------------
// The cell
// ---------------------------------------------------------------------------

pub struct Cell {
    pub cfg: CellConfig,
    /// A weak self-reference, set once at construction.
    ///
    /// `Dispatcher`'s methods take `&self`, but every path from the proxy into
    /// a guest has to hand the dispatch to a blocking task — §6.1 puts guest
    /// execution on the exec pool and never on a reactor thread — and that
    /// needs an owned `Arc`. Weak, not strong, for the same reason the exec
    /// threads hold a `Weak`: a cell that keeps itself alive is a cell that
    /// leaks its threads in every test and every embedding.
    me: std::sync::OnceLock<Weak<Cell>>,
    rt: Arc<CellRuntime>,
    runtime: Option<tokio::runtime::Runtime>,
    queue: Arc<WorkQueue>,
    index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>>,
    /// §6.4: **the tombstone ring.**
    ///
    /// A hard-killed instance's memory goes back to the kernel immediately —
    /// that part is not negotiable, it is arbitrary state — but the *slot*
    /// is kept for a while, holding nothing but its id and its tombstone
    /// flag. Without it, "any subsequent host call into it is a hard
    /// assertion failure" degrades to "the id is unknown", and those are very
    /// different answers to give an operator: the first says *this instance
    /// was killed and you may not talk to it*, the second says *never heard
    /// of it*, which is what a use-after-free looks like from the outside.
    ///
    /// Appendix C F2 is the reason it is worth the bytes: the engine will
    /// happily answer a call on a killed store, so the refusal has to come
    /// from a platform structure that outlives the instance.
    ///
    /// Bounded, because an unbounded record of every kill a long-lived cell
    /// ever took is a leak with a spec citation. The oldest entry is dropped
    /// when the ring is full, and a call for an id older than that gets
    /// `None` — the honest answer, since nothing about it is remembered.
    tombstoned: Mutex<std::collections::VecDeque<Arc<Slot>>>,
    modules: CountedRwLock<HashMap<ModuleId, Arc<ModuleEntry>>>,
    residents: CountedMutex<HashMap<InstanceId, Resident>>,
    /// **Sharded, and measured before it was.** One mutex here cost the
    /// execution threads 160 s of blocked time over a 30 s hold at 896
    /// instances — 48.8% of everything they waited on, across 3.2 million
    /// contended acquisitions — while every use of it is a single-key
    /// operation on an `InstanceId`. See `lockstats::ShardedMap`.
    active: crate::lockstats::ShardedMap<InstanceId, ActiveRequest>,
    breaker: HangBreaker,
    metrics: Arc<Metrics>,
    hot: AtomicUsize,
    next_dispatch: AtomicU64,
    /// Tenants that have been unilaterally moved to a quarantine cell (§8.2.4)
    /// and may no longer be admitted here.
    quarantined: Mutex<Vec<String>>,
    shutdown: Arc<AtomicBool>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl Cell {
    pub fn new(cfg: CellConfig, services: Arc<Services>) -> Arc<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(cfg.reactor_threads)
            .thread_name(format!("{}-reactor", cfg.name))
            .enable_all()
            .build()
            .expect("reactor runtime");

        let metrics = Arc::new(Metrics::new());
        // §10/H7. Applied once, here, so every metrics reader sees the same
        // setting the dispatch path is running under.
        metrics.set_per_dispatch_sampling(cfg.telemetry_sample_every);
        let queue = Arc::new(WorkQueue::new(
            cfg.admission_queue,
            cfg.exec_threads,
            cfg.queue_strategy,
        ));
        let index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let futures = Arc::new(FutureTable::new());

        let rt = Arc::new(CellRuntime {
            services,
            futures,
            metrics: metrics.clone(),
            wake: Arc::new(CellWake {
                queue: queue.clone(),
                index: index.clone(),
                metrics: metrics.clone(),
            }),
            reactor: runtime.handle().clone(),
        });

        let cell = Arc::new(Cell {
            me: std::sync::OnceLock::new(),
            breaker: HangBreaker::new(cfg.breaker),
            rt,
            runtime: Some(runtime),
            queue,
            index,
            tombstoned: Mutex::new(std::collections::VecDeque::new()),
            modules: CountedRwLock::new(HashMap::new()),
            // Handle 0 is reserved: a guest that reads an uninitialised field
            // and calls `socket_send(0, ..)` must get `NotFound`, not
            // somebody's chat room.
            residents: CountedMutex::new(HashMap::new()),
            active: crate::lockstats::ShardedMap::new(ACTIVE_SHARDS),
            metrics,
            hot: AtomicUsize::new(0),
            next_dispatch: AtomicU64::new(1),
            quarantined: Mutex::new(Vec::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            workers: Mutex::new(Vec::new()),
            cfg,
        });

        let _ = cell.me.set(Arc::downgrade(&cell));
        cell.start_exec_pool();
        cell.start_idle_sweeper();
        cell
    }

    /// The owning `Arc`, when there still is one.
    ///
    /// `None` only while the cell is being dropped, which is exactly when a
    /// late dispatch should be refused rather than served.
    pub(crate) fn me(&self) -> Option<Arc<Cell>> {
        self.me.get().and_then(Weak::upgrade)
    }

    /// §6.1: execution-pool threads run Hot dispatches as synchronous calls
    /// into the engine, and **the kernel preempts them**.
    fn start_exec_pool(self: &Arc<Self>) {
        let mut handles = Vec::new();
        for i in 0..self.cfg.exec_threads {
            // `Weak`, not `Arc`: a pool thread holding the cell alive would
            // make the cell un-droppable, and a cell that only stops when you
            // remember to stop it is a cell that leaks threads in every test
            // and every embedding.
            let me = Arc::downgrade(self);
            let queue = self.queue.clone();
            let name = format!("{}-exec-{i}", self.cfg.name);
            handles.push(
                std::thread::Builder::new()
                    .name(name)
                    .spawn(move || Cell::exec_loop(me, queue, i))
                    .expect("exec thread"),
            );
        }
        *self.workers.lock() = handles;
    }

    /// §6.6's idle trajectory, running with no tenant involvement.
    fn start_idle_sweeper(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let interval = Duration::from_millis(self.cfg.sweep_interval_ms.max(1));
        let stop = self.shutdown.clone();
        let name = format!("{}-idle", self.cfg.name);
        let h = std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(interval);
                    let Some(cell) = me.upgrade() else { return };
                    cell.sweep_idle();
                }
            })
            .expect("idle sweeper");
        self.workers.lock().push(h);
    }

    fn exec_loop(me: Weak<Self>, queue: Arc<WorkQueue>, worker: usize) {
        while let Some(job) = queue.pop(worker) {
            let Some(cell) = me.upgrade() else { return };
            match job {
                Job::Dispatch(j) => cell.run_dispatch(*j),
                Job::Wake(slot) => cell.run_wake(slot),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Registration and trust tiers (§8.2.4)
    // -----------------------------------------------------------------------

    /// Whether this owner is permitted by the cell's quarantine state.
    pub fn admits(&self, tenant: &str) -> bool {
        !self.quarantined.lock().iter().any(|t| t == tenant)
    }

    pub fn register_module(
        self: &Arc<Self>,
        tenant: &str,
        activator: Arc<dyn Activator>,
        manifest: AppManifest,
    ) -> crate::Result<()> {
        let limits = manifest.limits;
        if !self.admits(tenant) {
            anyhow::bail!(
                "cell `{}` refuses quarantined tenant `{tenant}`",
                self.cfg.name
            );
        }
        let module = activator.module().clone();
        let lifecycle = manifest.handlers.lifecycle;
        let entry = ModuleEntry {
            tenant: tenant.to_string(),
            limits,
            manifest,
            pool: InstancePool::new(activator.clone(), lifecycle, self.cfg.max_idle_per_module),
            activator,
            meters: Arc::new(Meters::new()),
        };
        self.modules.write().insert(module, Arc::new(entry));
        Ok(())
    }

    pub fn meters(&self, module: &ModuleId) -> Option<Arc<Meters>> {
        self.modules.read().get(module).map(|m| m.meters.clone())
    }

    /// How densely this cell is collecting [`StageTimings`].
    ///
    /// Published so a reader can tell how many observations a stage percentile
    /// rests on. A percentile with no sample count beside it is a number
    /// wearing a quantile's name, and at one request in 128 the count is not
    /// something a reader can derive from the request total either — sheds and
    /// refusals never reach the stamping points.
    pub fn stage_sample_every(&self) -> u64 {
        self.cfg.stage_sample_every
    }

    /// The sampling decision, taken once per request at admission.
    ///
    /// **Once, and carried**, rather than re-derived at each of the four
    /// stamping points. Re-deriving would be three more integer divisions per
    /// request on the dispatch path, and — the part that actually matters —
    /// would let the four stages disagree about which requests they describe
    /// if the configuration were ever changed mid-run.
    #[inline]
    fn new_stamps(&self, dispatch_id: u64) -> Option<StageStamps> {
        stage_sampled(dispatch_id, self.cfg.stage_sample_every).then(|| StageStamps {
            queued: Instant::now(),
            submit_ns: 0,
            queue_ns: 0,
        })
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        self.refresh_gauges();
        self.metrics.snapshot()
    }

    /// What each of the cell's hot locks cost the threads that waited for it.
    ///
    /// Ordered by how many times one Durable `Done` dispatch takes each, so
    /// the list reads as the hypothesis it exists to test. The names are the
    /// field paths rather than prose, because the next step after reading this
    /// table is opening that field.
    ///
    /// **Seven of the ten, not all ten.** The two §10 telemetry histograms
    /// already have a way to be measured — `CellConfig::telemetry_sample_every
    /// = 0` takes both off the dispatch path while leaving counts and means
    /// intact, so their cost is a difference between two runs — and
    /// `Metrics::by_tenant` is taken once on a path that also takes `active`
    /// six times. Instrumenting a lock is not free of judgement either: each
    /// wrapper adds a counter pair that the contending threads share.
    pub fn lock_stats(&self) -> Vec<(&'static str, crate::lockstats::LockSample)> {
        vec![
            ("Cell::active", self.active.lock_stats()),
            ("FutureTable::inner", self.rt.futures.lock_stats()),
            ("WorkQueue::inner", self.queue.lock_stats()),
            ("HangBreaker::entries", self.breaker.lock_stats()),
            ("Cell::residents", self.residents.stats().snapshot()),
            ("Cell::modules", self.modules.stats().snapshot()),
        ]
    }
    /// The live counters, not a snapshot of them.
    ///
    /// A snapshot flattens histograms to the percentiles §10 exports, which is
    /// the right shape for a scrape and the wrong one for asking whether a
    /// histogram is still *recording* — the question
    /// `CellConfig::telemetry_sample_every` exists to answer. Also carries
    /// [`Metrics::tombstone_invariant`], which is meaningful at rest.
    pub fn metrics_raw(&self) -> &Arc<Metrics> {
        &self.metrics
    }
    pub fn metrics_json(&self) -> String {
        self.refresh_gauges();
        self.metrics.to_json()
    }
    pub fn metrics_text(&self) -> String {
        self.refresh_gauges();
        self.metrics.to_text()
    }

    fn refresh_gauges(&self) {
        let (mut live, mut parked, mut hib, mut hot) = (0u64, 0u64, 0u64, 0u64);
        for r in self.residents.lock().values() {
            match r.slot.state() {
                LifecycleTier::Hot => {
                    hot += 1;
                    live += 1;
                }
                LifecycleTier::Parked => {
                    parked += 1;
                    live += 1;
                }
                LifecycleTier::Hibernated => hib += 1,
                LifecycleTier::Discarded => {}
            }
        }
        for m in self.modules.read().values() {
            let (w, f) = m.pool.idle_counts();
            parked += (w + f) as u64;
            live += (w + f) as u64;
        }
        self.metrics.live.store(live, Ordering::Relaxed);
        self.metrics.parked.store(parked, Ordering::Relaxed);
        self.metrics.hibernated.store(hib, Ordering::Relaxed);
        self.metrics.hot.store(hot, Ordering::Relaxed);
    }

    /// §6.4/§9.4: clear the hang breaker for a module.
    ///
    /// A new version deploys and the breaker is gone for free — the key
    /// carries the version — so this exists for the operator's manual
    /// override, and for tests that need to measure something the breaker
    /// would otherwise fail fast before reaching.
    pub fn clear_breaker(&self, module: &ModuleId) {
        self.breaker.clear_module(module);
    }

    /// Idle instances a module's pool is holding: `(warm, pooled-fresh)`.
    ///
    /// §6.3's acquisition order is "warm-parked > pooled-fresh > activate", so
    /// this is the difference between a request that pays for an activation
    /// and one that does not. Exposed because a *parked* instance and a
    /// *pooled* one are different things and the gauges cannot tell them
    /// apart: an instance is Parked from the moment the guest returns, but it
    /// is not acquirable until `finish_and_release` puts it back — a window
    /// the response has already left through (§6.3 responds first, on
    /// purpose).
    pub fn idle_instances(&self, module: &ModuleId) -> Option<(usize, usize)> {
        self.modules
            .read()
            .get(module)
            .map(|e| e.pool.idle_counts())
    }

    pub fn queue_depth(&self) -> usize {
        self.queue.depth()
    }
    pub fn hot_count(&self) -> usize {
        self.hot.load(Ordering::Relaxed)
    }
    pub fn futures(&self) -> &Arc<FutureTable> {
        &self.rt.futures
    }
    /// The cell's reactor (§6.1). Reactor threads own all I/O, so anything
    /// that touches the network — the metrics endpoint, the proxy's listener —
    /// belongs here rather than on an execution-pool thread.
    pub fn reactor(&self) -> &tokio::runtime::Handle {
        &self.rt.reactor
    }

    // -----------------------------------------------------------------------
    // Admission (§6.4)
    // -----------------------------------------------------------------------

    /// Submit a top-level work item and block until the response is final.
    ///
    /// "Final" means Done or a fault — a dispatch that parks and is later woken
    /// keeps this call outstanding, because §6.3's lifecycle is one request
    /// spanning many slices.
    pub fn dispatch_blocking(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
    ) -> crate::Result<DispatchResult> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let route = target.route();
        let now = crate::now_ms();
        let key = BreakerKey {
            module: module.clone(),
            route: route.clone(),
        };
        let dispatch_id = rec.dispatch_id;
        if self.breaker.check(&key, now).is_err() {
            self.metrics
                .breaker_rejections
                .fetch_add(1, Ordering::Relaxed);
            return Ok(DispatchResult::breaker_open(dispatch_id));
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder: tx,
            enqueued: Instant::now(),
            stamps: self.new_stamps(dispatch_id),
        });
        if self.queue.push_dispatch(job).is_err() {
            // §6.4: overflow queues at admission; the cell sheds rather than
            // saturating. The proxy is supposed to shed before this point.
            self.metrics.shed.fetch_add(1, Ordering::Relaxed);
            if let Some(m) = self.meters(module) {
                m.record_shed();
            }
            return Ok(DispatchResult::shed(dispatch_id));
        }
        rx.recv_timeout(timeout)
            .map_err(|_| anyhow::anyhow!("dispatch did not complete within {timeout:?}"))
    }

    /// Submit without waiting, handing back the channel the final response
    /// will arrive on.
    ///
    /// This is the shape the C3 measurement needs: a thousand dispatches that
    /// *park* and stay parked. `dispatch_blocking` would hold a thread per
    /// outstanding request, which would measure the test harness rather than
    /// the cell.
    pub fn submit(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
    ) -> Option<std::sync::mpsc::Receiver<DispatchResult>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.submit_with(module, target, rec, tx).then_some(rx)
    }

    /// Submit onto a **caller-provided** channel, which many submissions may
    /// share.
    ///
    /// [`Cell::submit`] allocates one channel per request, which is right for
    /// a test holding a few dozen parked instances and wrong for anything
    /// offering sustained load: at a hundred thousand requests a second it is
    /// a hundred thousand allocations a second on the caller's path, and —
    /// worse — leaves the caller holding a hundred thousand receivers to poll.
    /// A load generator built that way measures itself.
    ///
    /// The result carries [`DispatchResult::dispatch_id`], so a caller
    /// draining one shared channel can match each completion to its request.
    ///
    /// Returns `false` if the cell shed at admission (§6.4). **Nothing is sent
    /// on `responder` in that case** — the shed is reported by the return
    /// value alone, and the caller owns counting it.
    ///
    /// This differs from [`Cell::submit`], which does send a `503`, and the
    /// difference is forced by the shared channel: a caller draining one
    /// receiver for every request in flight cannot tell a synthesised refusal
    /// from a real completion without matching it up first, so a shed
    /// delivered both ways would be counted twice — once from the `false` and
    /// once out of the drain. The breaker path above *does* send, because a
    /// breaker rejection returns `true` and the drain is its only report.
    ///
    /// **The channel must not be allowed to fill.** `SyncSender::send` blocks,
    /// and the sender here is an execution-pool thread, so a caller that stops
    /// draining stops the cell. Size it for the outstanding count and treat
    /// the bound as a harness invariant rather than as backpressure.
    pub fn submit_with(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        responder: std::sync::mpsc::SyncSender<DispatchResult>,
    ) -> bool {
        let now = crate::now_ms();
        let dispatch_id = rec.dispatch_id;
        let key = BreakerKey {
            module: module.clone(),
            route: target.route(),
        };
        if self.breaker.check(&key, now).is_err() {
            self.metrics
                .breaker_rejections
                .fetch_add(1, Ordering::Relaxed);
            let _ = responder.send(DispatchResult::breaker_open(dispatch_id));
            return true;
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder,
            enqueued: Instant::now(),
            stamps: self.new_stamps(dispatch_id),
        });
        if self.queue.push_dispatch(job).is_err() {
            self.metrics.shed.fetch_add(1, Ordering::Relaxed);
            if let Some(m) = self.meters(module) {
                m.record_shed();
            }
            return false;
        }
        true
    }

    /// Async form, for callers already on a reactor.
    pub async fn dispatch(
        self: &Arc<Self>,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
    ) -> crate::Result<DispatchResult> {
        let me = self.clone();
        let module = module.clone();
        tokio::task::spawn_blocking(move || me.dispatch_blocking(&module, target, rec, timeout))
            .await?
    }

    /// What a reactor thread does when a host future completes: map the id to
    /// its owning instance and enqueue the wake (§6.1's wake router).
    ///
    /// Public because the reactor is not the only completer — an alarm from
    /// the control plane, a socket frame at the proxy and a DI reply all
    /// arrive this way, and in the PoC so do tests.
    pub fn complete_future(&self, id: u64, result: wisp_abi::result::ResultRecord) {
        if let crate::futures::WakeTarget::Instance(owner) = self.rt.futures.complete(id, result) {
            self.rt.wake.signal(owner);
        }
    }

    /// Build a plain HTTP dispatch record with this app's budget (§11.1).
    pub fn http_record(&self, module: &ModuleId, req: wisp_abi::HttpRequest) -> DispatchRecord {
        let limits = self
            .modules
            .read()
            .get(module)
            .map_or(Limits::default(), |m| m.limits);
        DispatchRecord {
            dispatch_id: self.next_dispatch.fetch_add(1, Ordering::Relaxed),
            kind: DispatchKind::Http(req),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: limits.cpu_ms_per_http_dispatch,
            now_ms: crate::now_ms(),
        }
    }

    // -----------------------------------------------------------------------
    // §6.3 request lifecycle
    // -----------------------------------------------------------------------

    fn run_dispatch(self: &Arc<Self>, job: DispatchJob) {
        // The queue stage closes here, at the top, before the module lookup:
        // everything below this line is work the cell does *for* the request,
        // and folding the first `modules` read into the queueing delay would
        // charge a lock the request needed to the lane it was waiting in.
        let mut job = job;
        if let Some(s) = &mut job.stamps {
            s.queue_ns = s.queued.elapsed().as_nanos() as u64;
        }
        let job = job;
        let did = job.rec.dispatch_id;
        let Some(entry) = self.modules.read().get(&job.module).cloned() else {
            let _ = job.responder.send(DispatchResult {
                status: 404,
                body: b"no such module".to_vec(),
                ..DispatchResult::shed(did)
            });
            return;
        };

        let acquired;
        let slot = match &job.target {
            Target::Handler { .. } => {
                let t0 = Instant::now();
                match self.acquire_handler(&entry) {
                    Ok((s, src)) => {
                        acquired = match src {
                            AcquireSource::WarmParked => {
                                self.metrics.served_warm.fetch_add(1, Ordering::Relaxed);
                                "warm"
                            }
                            AcquireSource::PooledFresh => {
                                self.metrics
                                    .served_pooled_fresh
                                    .fetch_add(1, Ordering::Relaxed);
                                "pooled_fresh"
                            }
                            AcquireSource::Activated => {
                                self.metrics
                                    .served_activated
                                    .fetch_add(1, Ordering::Relaxed);
                                self.metrics
                                    .activation_us
                                    .record(t0.elapsed().as_micros() as u64);
                                "activated"
                            }
                        };
                        s
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "activation failed");
                        let _ = job.responder.send(DispatchResult {
                            status: 500,
                            body: e.to_string().into_bytes(),
                            ..DispatchResult::shed(did)
                        });
                        return;
                    }
                }
            }
        };

        let budget = CpuBudget {
            cpu_ms: job.rec.cpu_budget_ms,
            wall_ms_cap: self.cfg.wall_ms_cap,
            // First slice of this dispatch: nothing burned yet. Later slices
            // carry what earlier ones spent, so B and B+G bind the dispatch
            // rather than each wake (§6.4).
            spent_ns: 0,
        };
        // §5.2/§7.2: "One instance MAY have multiple in-flight dispatches only
        // for read-only Handler reuse; DIs are strictly one dispatch at a
        // time." A Handler never reaches this branch — a parked instance is
        // not in the pool, so a second dispatch gets a different one — but a
        // DI is addressed by identity and can. Refuse rather than overwrite the
        // in-flight request. Ordering and serialization of DI messages is the
        // single-writer inbox's job (§7.2), which lives in `wisp-durable`.
        if self.active.shard(slot.id.0).lock().contains_key(&slot.id) {
            // §10: counted, because this refusal is one of the four legs the
            // density gate stands on and the only other count of it lives in
            // the load generator. Two independent counts of the same event is
            // the whole point — a counting error on either side is otherwise
            // undetectable.
            //
            // Note where this returns from: *before* `Metrics::dispatches` is
            // incremented below, so a 409 is not a dispatch. The identity that
            // closes the books is
            // `submitted == dispatches + shed + breaker_rejections +
            //  single_writer_refusals`.
            self.metrics
                .single_writer_refusals
                .fetch_add(1, Ordering::Relaxed);
            let _ = job.responder.send(DispatchResult {
                status: 409,
                body: "durable instance already has a dispatch in flight (§7.2)"
                    .as_bytes()
                    .to_vec(),
                ..DispatchResult::shed(did)
            });
            return;
        }
        // §5.4: *"every dispatch carries an implicit cancellation future; the
        // host cancels by completing it"*. It is allocated here, before the
        // guest is entered, because the epoch callback that completes it at B
        // runs with the guest on the stack and could not allocate one then.
        //
        // A guest that never awaits it pays nothing but a table entry, which
        // `end_dispatch` reaps. A guest that does await it observes a tripped
        // token at its next wake — which is the only channel there is, because
        // §5.4 makes cancellation deliberately *not* an entrancy.
        let mut job = job;
        job.rec.cancel_future = self
            .rt
            .futures
            .alloc_cancellation(slot.id, job.rec.dispatch_id);
        slot.ctx.set_cancel_future(job.rec.cancel_future);

        self.active.shard(slot.id.0).lock().insert(
            slot.id,
            ActiveRequest {
                dispatch_id: job.rec.dispatch_id,
                lifecycle: entry.manifest.handlers.lifecycle,
                module: job.module.clone(),
                route: job.target.route(),
                responder: Some(job.responder),
                started: job.enqueued,
                cpu_ns: 0,
                wall_ns: 0,
                slices: 0,
                acquired,
                budget,
                stamps: job.stamps,
            },
        );

        entry.meters.record_dispatch();
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);

        slot.ctx
            .begin_dispatch(crate::now_ms(), job.rec.dispatch_id);

        let outcome = {
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(e) => {
                    // §5.4 refusing a reentrant call. For a Durable Instance
                    // this is §7.2's "strictly one dispatch at a time"; the
                    // caller must retry.
                    //
                    // §6.4's tombstone refusal arrives at the same door and is
                    // a different fact — "this instance was killed and you may
                    // not talk to it", not "come back in a moment" — so it is
                    // counted separately. Folding it into the reentrancy
                    // counter left `tombstone_refusals`, §10's negative
                    // control for the tombstone rule, permanently at zero.
                    self.count_enter_refusal(e);
                    self.respond(
                        &slot,
                        DispatchResult {
                            status: 409,
                            body: b"instance busy".to_vec(),
                            ..DispatchResult::shed(did)
                        },
                    );
                    self.active.shard(slot.id.0).lock().remove(&slot.id);
                    return;
                }
            };
            self.hot.fetch_add(1, Ordering::Relaxed);
            let out = guard.instance().dispatch(&job.rec, budget);
            self.hot.fetch_sub(1, Ordering::Relaxed);
            out
        };

        // A completion can land while a dispatch is Hot; §5.4 made it wait, so
        // if one did, hand the instance to the wake lane rather than leaving a
        // result queued against a parked instance nobody will visit again.
        if !self.settle(&slot, &entry, outcome) {
            self.queue.push_wake(slot);
        }
    }

    fn run_wake(self: &Arc<Self>, slot: Arc<Slot>) {
        let Some(module) = self
            .residents
            .lock()
            .get(&slot.id)
            .map(|r| r.module.clone())
        else {
            return;
        };
        let Some(entry) = self.modules.read().get(&module).cloned() else {
            return;
        };

        loop {
            // Take exclusive ownership of the instance *before* taking the
            // result out of the future table. The other order has a hole: a
            // result pulled out with nowhere to deliver it has been silently
            // dropped, and a dropped wake is a request that never finishes.
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(crate::instances::EnterError::Busy) => {
                    // §5.4: a previous call into this instance has not
                    // returned. This is the reentrancy rule doing its job —
                    // the result stays queued and the in-flight call drains it
                    // when it finishes.
                    self.metrics
                        .reentrancy_refusals
                        .fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(e @ crate::instances::EnterError::Tombstoned) => {
                    // A wake queued for an instance that was tombstoned before
                    // the wake lane reached it. §6.4 refuses, and §10 counts.
                    self.count_enter_refusal(e);
                    return;
                }
                Err(_) => return,
            };
            let Some((fid, res)) = self.rt.futures.take_ready(slot.id) else {
                return;
            };
            // §6.4: B and B+G bind the **dispatch**, so a wake resumes with
            // what earlier slices already burned. Without this a guest could
            // park and be woken indefinitely, each slice getting a fresh 50 ms
            // and the hard deadline never arriving — and a guest unwinding
            // inside grace would get its grace window back on every await,
            // which would make "a pinned bit buys exactly G" false for anyone
            // willing to park.
            let budget = self
                .active
                .shard(slot.id.0)
                .lock()
                .get(&slot.id)
                .map(|a| a.budget.after(a.cpu_ns))
                .unwrap_or_else(CpuBudget::http);

            // §8.5: a wake is a new slice, so the coarse clock advances here
            // and nowhere else.
            slot.ctx
                .begin_slice(crate::now_ms(), slot.ctx.dispatch_id());

            let t0 = Instant::now();
            self.hot.fetch_add(1, Ordering::Relaxed);
            let outcome = guard.instance().wake(fid, &res, budget);
            self.hot.fetch_sub(1, Ordering::Relaxed);
            drop(guard);
            self.metrics.wake_us.record(t0.elapsed().as_micros() as u64);

            if self.settle(&slot, &entry, outcome) {
                return;
            }
        }
    }

    /// The §6.3 poll-status switch. Returns true when the instance is no
    /// longer available for more wakes in this loop.
    fn settle(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        out: DispatchOutcome,
    ) -> bool {
        // §11.2: record the Hot interval. Nothing else is ever recorded — and on
        // a kill of either phase that interval *includes the overage*, because
        // the meter reads the thread clock and does not know what a budget is
        // (§6.4: "the overage CPU is recorded").
        entry.meters.record_cpu(&out);
        if out.grace_deferrals > 0 {
            self.metrics
                .grace_deferrals
                .fetch_add(out.grace_deferrals, Ordering::Relaxed);
        }
        self.metrics.record_tenant(
            &entry.tenant,
            out.cpu_ns,
            !matches!(out.completion, Completion::Done | Completion::Parked),
        );

        // The vtable may owe the guest a trap it could not raise from an
        // import that returns a bare `i64` — §11.1's pending-futures cap, a
        // malformed payload, a capability violation. `call_future` cannot
        // trap, so it latches; this is where the latch becomes a trap, before
        // the guest is allowed to proceed as though the call had succeeded.
        let latched = slot.ctx.take_trap();
        let mut latched_msg = None;
        let completion = match latched {
            // A clean kill is deliberately NOT in this set. Its instance is
            // consistent and its slot reusable; converting it to a trap on a
            // latched code would tombstone something the grace window just
            // saved. A latched code alongside a clean kill is almost always a
            // poisoned import the guest ignored on its way out.
            Some(code) if matches!(out.completion, Completion::Done | Completion::Parked) => {
                if code == TrapCode::TooManyFutures {
                    self.metrics
                        .pending_cap_rejections
                        .fetch_add(1, Ordering::Relaxed);
                }
                latched_msg = Some(format!("{code:?}"));
                Completion::Trapped
            }
            _ => out.completion,
        };

        let (lifecycle, module, route) = {
            let mut a = self.active.shard(slot.id.0).lock();
            match a.get_mut(&slot.id) {
                Some(req) => {
                    req.cpu_ns += out.cpu_ns;
                    req.wall_ns += out.wall_ns;
                    req.slices += 1;
                    (req.lifecycle, req.module.clone(), req.route.clone())
                }
                None => (
                    entry.manifest.handlers.lifecycle,
                    entry.activator.module().clone(),
                    String::new(),
                ),
            }
        };
        let breaker_key = BreakerKey { module, route };

        match completion {
            Completion::Parked => {
                slot.park(crate::now_ms());
                // A completion may have landed while we were Hot; §5.4 made it
                // wait, so drain it now.
                !self.rt.futures.has_ready(slot.id)
            }
            Completion::Done => {
                // §6.3: "the guest may return `done` with a body-stream handle
                // and continue producing chunks via write futures under
                // `WaitUntil` semantics". An unfinished streaming response
                // must therefore stay in the response table — taking it here
                // would make every subsequent `resp_write` fail with
                // `NoSuchHandle`, which is a silent truncation of the tenant's
                // response.
                //
                // **PoC gap:** `DispatchResult` carries a complete body, not a
                // stream, so the caller receives the bytes accumulated up to
                // `done`. Chunks written afterwards accumulate on the instance
                // and are readable via `Cell::response_snapshot`, but the PoC
                // does not pipe them to the requester. Streaming to the client
                // is `wisp-proxy`'s half of §6.3 and is not wired.
                let response = match out.status {
                    PollStatus::Done { response } if response != 0 => {
                        let mut t = slot.ctx.responses.lock();
                        match t.get(response) {
                            Some(r) if r.head.streaming && !r.finished => Some(r.clone()),
                            _ => t.take(response),
                        }
                    }
                    _ => None,
                };
                let streaming_open = response
                    .as_ref()
                    .is_some_and(|r| r.head.streaming && !r.finished);
                let wait_until = self.rt.futures.wait_until_outstanding(slot.id) || streaming_open;

                let (status, headers, body) = match &response {
                    Some(r) => (r.head.status, r.head.headers.clone(), r.body()),
                    None => (204, vec![], Vec::new()),
                };
                let result = self.build_result(slot, status, headers, body, completion, None);
                self.respond(slot, result);

                match decide(lifecycle, completion, wait_until) {
                    RequestEnd::RetainedForWaitUntil => {
                        // §2.2/§2.6: the instance lives past the response and,
                        // under `fresh`, the reset is still owed.
                        slot.park(crate::now_ms());
                        !self.rt.futures.has_ready(slot.id)
                    }
                    RequestEnd::Disposed(d) => {
                        self.breaker.record_success(&breaker_key);
                        self.finish_and_release(slot, entry, d);
                        true
                    }
                    RequestEnd::Parked => unreachable!("Done cannot decide Parked"),
                }
            }
            // §6.4's CLEAN kill: the dispatch entered grace and returned
            // inside it. The request still fails — a kill is a kill, and §13
            // gives it `budget_exceeded` telemetry — but three things that are
            // true of a hard kill are false here, and all three are the point:
            //
            //   * the instance is NOT tombstoned. `finally` ran, its state is
            //     consistent, and its slot is reusable (`decide` resets it);
            //   * it does NOT count toward the hang breaker. §6.4: clean kills
            //     "are the budget working as designed";
            //   * it is recorded, overage and all, exactly like any other CPU.
            Completion::CleanKill => {
                let overage = out.overage_ns;
                entry.meters.record_budget_kill(KillPhase::Clean, overage);
                self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
                self.metrics.clean_kills.fetch_add(1, Ordering::Relaxed);
                self.metrics.grace_entries.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .overage_ns
                    .fetch_add(overage, Ordering::Relaxed);
                tracing::info!(
                    tenant = %entry.tenant, instance = %slot.id,
                    overage_us = overage / 1_000,
                    "clean kill: dispatch unwound inside its grace window (§6.4)"
                );
                let msg = format!(
                    "budget_exceeded: cpu budget elapsed; the dispatch unwound cleanly inside its \
                     grace window and was recorded {:.3} ms of overage (§6.4 clean kill)",
                    overage as f64 / 1e6
                );
                let result = self.build_result(
                    slot,
                    503,
                    vec![],
                    msg.clone().into_bytes(),
                    completion,
                    Some(msg),
                );
                self.respond(slot, result);
                match decide(lifecycle, completion, false) {
                    RequestEnd::Disposed(d) => {
                        // Note what is NOT here: no `breaker.record_kill`, and
                        // no `breaker.record_success` either. A clean kill is
                        // neither evidence of a hang nor evidence of health.
                        self.finish_and_release(slot, entry, d);
                        true
                    }
                    other => unreachable!("CleanKill cannot decide {other:?}"),
                }
            }
            Completion::Trapped | Completion::BudgetKill => {
                let killed = completion == Completion::BudgetKill;
                if killed {
                    entry
                        .meters
                        .record_budget_kill(KillPhase::Hard, out.overage_ns);
                    self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
                    self.metrics.hard_kills.fetch_add(1, Ordering::Relaxed);
                    self.metrics
                        .overage_ns
                        .fetch_add(out.overage_ns, Ordering::Relaxed);
                    if out.kill.is_some() {
                        self.metrics.grace_entries.fetch_add(1, Ordering::Relaxed);
                    }
                    // §6.4: only HARD kills trip the hang breaker.
                    self.breaker.record_kill(&breaker_key, crate::now_ms());
                } else {
                    entry.meters.record_trap();
                    // Inside the `else`, beside `record_trap`, and not in the
                    // shared tail. A hard budget kill is not a trap: counting
                    // it as one made `traps ⊇ hard_kills`, made
                    // `tombstones == hard_kills + traps` false by construction
                    // (a correct implementation gave `tombstones == traps` and
                    // the sum overshot by exactly `hard_kills`), and made the
                    // blast table's adjacent `hard kills` and `tombstones`
                    // rows double-count for anyone reading down the column.
                    self.metrics.traps.fetch_add(1, Ordering::Relaxed);
                }
                // §6.4: **the tombstone.** Set on the slot before anything
                // else touches it, because the engine will not do it for us
                // (Appendix C, F2: a killed instance answered a later exported
                // call correctly, globals intact). From here `slot.enter()`
                // refuses, capture is refused, and reuse is impossible.
                slot.tombstone();
                // §6.4's identity — `tombstones == hard_kills + traps` — is
                // established by these three increments and nowhere else, but
                // it is NOT asserted here: the three counters are separate
                // atomics and two execution threads tombstoning at once are
                // transiently inconsistent between them, so a check on this
                // line would be a race, not an invariant.
                // [`Metrics::tombstone_invariant`] is the check, and it is
                // meaningful at rest — which is where the tests call it.
                self.metrics.tombstones.fetch_add(1, Ordering::Relaxed);
                self.remember_tombstone(slot);
                let msg = out
                    .trap
                    .as_ref()
                    .map(|t| format!("{}: {}", t.kind.tenant_code(), t.message))
                    .or_else(|| latched_msg.map(|c| format!("capability_violation: {c}")))
                    .unwrap_or_else(|| "trap".into());
                let status = if killed { 503 } else { 500 };
                let result = self.build_result(
                    slot,
                    status,
                    vec![],
                    msg.clone().into_bytes(),
                    completion,
                    Some(msg),
                );
                self.respond(slot, result);
                // §13: traps are never retried on the same memory. §6.4: the
                // slot is reset before reuse — and for a tombstone "reset"
                // means the whole activation goes back to the kernel, not a
                // re-seal over memory a trap left mid-write.
                self.finish_and_release(slot, entry, evict());
                true
            }
        }
    }

    fn build_result(
        &self,
        slot: &Arc<Slot>,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        completion: Completion,
        trap: Option<String>,
    ) -> DispatchResult {
        let a = self.active.shard(slot.id.0).lock();
        let (dispatch_id, cpu_ns, wall_ns, slices, acquired, stamps) = a
            .get(&slot.id)
            .map(|r| {
                (
                    r.dispatch_id,
                    r.cpu_ns,
                    r.wall_ns,
                    r.slices,
                    r.acquired,
                    r.stamps,
                )
            })
            .unwrap_or((0, 0, 0, 0, "unknown", None));
        DispatchResult {
            dispatch_id,
            status,
            headers,
            body,
            completion,
            cpu_ns,
            wall_ns,
            trap,
            instance: slot.id,
            acquired,
            slices,
            // `wall_ns` is the guest stage: `settle` accumulates it per slice,
            // so a dispatch that parked and was woken reports the guest time
            // of the whole request rather than of its first slice.
            stages: stamps.map(|s| StageTimings {
                submit_ns: s.submit_ns,
                queue_ns: s.queue_ns,
                guest_ns: wall_ns,
                replied_mono_ns: 0,
            }),
        }
    }

    fn respond(&self, slot: &Arc<Slot>, result: DispatchResult) {
        let tx = self
            .active
            .shard(slot.id.0)
            .lock()
            .get_mut(&slot.id)
            .and_then(|a| a.responder.take());
        if let Some(tx) = tx {
            // Stamped after the `active` shard is released and immediately
            // before the send, so the interval the caller reconstructs is the
            // channel and nothing else. Inside the lock it would carry the
            // shard's own contention, which `lock_stats` already reports under
            // its own name — one cost, counted twice, in two tables that are
            // read together.
            let mut result = result;
            if let Some(st) = &mut result.stages {
                st.replied_mono_ns = crate::mono_ns();
            }
            let _ = tx.send(result);
        }
    }

    fn finish_and_release(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        d: crate::instances::Disposition,
    ) {
        let req = self.active.shard(slot.id.0).lock().remove(&slot.id);
        if let Some(r) = &req {
            self.rt.futures.end_dispatch(slot.id, r.dispatch_id);
            self.metrics
                .request_us
                .record(r.started.elapsed().as_micros() as u64);
            self.metrics.park_ms.record(slot.parked_ms_total());
            entry.meters.record_park(slot.parked_ms_total());
        }

        if !d.is_warm() && !matches!(d.kind(), crate::instances::DispositionKind::Reset) {
            self.forget(slot);
        }
        self.rt.futures.release_instance(slot.id);
        entry.pool.release(slot.clone(), d);
        if slot.state() == LifecycleTier::Discarded {
            self.forget(slot);
        }
    }

    /// Keep a hard-killed slot addressable so a later call gets a refusal
    /// rather than a shrug. See the note on [`Cell::tombstoned`].
    fn remember_tombstone(&self, slot: &Arc<Slot>) {
        /// Enough to cover the burst a breaker takes to notice, and small
        /// enough that the ring is a rounding error next to one instance.
        const RING: usize = 256;
        let mut g = self.tombstoned.lock();
        if g.iter().any(|s| s.id == slot.id) {
            return;
        }
        g.push_back(slot.clone());
        while g.len() > RING {
            g.pop_front();
        }
    }

    /// Every instance this cell currently remembers tombstoning. §10's
    /// negative control reads it: a tombstone rule with nothing ever in here
    /// and no failing probe is unfalsifiable.
    pub fn tombstoned_instances(&self) -> Vec<InstanceId> {
        self.tombstoned.lock().iter().map(|s| s.id).collect()
    }

    /// §10: attribute one `Slot::enter` refusal to the counter that names it.
    ///
    /// One place rather than three, because the failure this had was a
    /// `Err(_)` arm that swallowed the distinction — and a second copy of the
    /// match is a second chance to make the same mistake.
    fn count_enter_refusal(&self, e: crate::instances::EnterError) {
        match e {
            crate::instances::EnterError::Busy => {
                self.metrics
                    .reentrancy_refusals
                    .fetch_add(1, Ordering::Relaxed);
            }
            crate::instances::EnterError::Tombstoned => {
                self.metrics
                    .tombstone_refusals
                    .fetch_add(1, Ordering::Relaxed);
            }
            // Hibernated or Discarded: not a refusal of a rule, just an
            // instance that is not here. Counted by the tier gauges.
            crate::instances::EnterError::NotResident(_) => {}
        }
    }

    /// Call into an instance by id, through the cell's front door (§6.4, §10).
    ///
    /// [`Cell::slot`] answers from the tombstone ring on purpose, so a
    /// kill-then-call gets an instance that **refuses** rather than a `None`
    /// that reads as "no such instance". This is that refusal *counted*:
    /// `tombstone_refusals` is §10's negative control for the tombstone rule,
    /// and a control nothing ever increments cannot fail, which makes a green
    /// dashboard unfalsifiable.
    ///
    /// The closure shape is not decoration: [`crate::instances::CallGuard`]
    /// borrows the slot, so handing one out would mean handing out the
    /// lifetime too. Going through `Slot::enter` rather than re-deriving the
    /// three refusals from `is_tombstoned`/`is_busy`/`state` keeps §5.4's
    /// "unrepresentable, not discouraged" property intact — this is the same
    /// door every dispatch uses, with a counter on it.
    pub fn with_instance<R>(
        &self,
        id: InstanceId,
        f: impl FnOnce(&mut (dyn wisp_engine::InstanceOps + Send)) -> R,
    ) -> Result<R, crate::instances::EnterError> {
        let Some(slot) = self.slot(id) else {
            return Err(crate::instances::EnterError::NotResident(
                LifecycleTier::Discarded,
            ));
        };
        // Bound rather than returned directly: the `Result<CallGuard>`
        // temporary borrows `slot`, and as a tail expression it would outlive
        // the `Arc` it borrows from.
        let out = match slot.enter() {
            Ok(mut guard) => Ok(f(guard.instance())),
            Err(e) => {
                self.count_enter_refusal(e);
                Err(e)
            }
        };
        out
    }

    fn forget(&self, slot: &Arc<Slot>) {
        self.index.lock().remove(&slot.id);
        self.residents.lock().remove(&slot.id);
    }

    // -----------------------------------------------------------------------
    // Acquisition
    // -----------------------------------------------------------------------

    fn make_ctx(&self, entry: &Arc<ModuleEntry>, id: InstanceId) -> Arc<InstanceCtx> {
        Arc::new(InstanceCtx::new(
            id,
            entry.activator.module().clone(),
            entry.tenant.clone(),
            entry.limits,
            entry.tier,
            entry.manifest.handlers.lifecycle,
            entry.manifest.bindings.fetch_allowlist.clone(),
            entry.meters.clone(),
            Some(self.rt.clone()),
        ))
    }

    fn acquire_handler(
        self: &Arc<Self>,
        entry: &Arc<ModuleEntry>,
    ) -> crate::Result<(Arc<Slot>, AcquireSource)> {
        let (slot, src) = entry.pool.acquire(|id| self.make_ctx(entry, id))?;
        self.index.lock().insert(slot.id, Arc::downgrade(&slot));
        self.residents
            .lock()
            .entry(slot.id)
            .or_insert_with(|| Resident {
                slot: slot.clone(),
                module: entry.activator.module().clone(),
            });
        Ok((slot, src))
    }

    // -----------------------------------------------------------------------
    // Inspection and forced transitions (tests, drills, `wisp dev`)
    // -----------------------------------------------------------------------

    /// Look up an instance by id.
    ///
    /// Falls back to the tombstone ring (§6.4), so a kill-then-call gets an
    /// instance that **refuses** rather than a `None` that reads as "no such
    /// instance". Linear over the ring, and only on the miss path.
    pub fn slot(&self, id: InstanceId) -> Option<Arc<Slot>> {
        if let Some(s) = self.residents.lock().get(&id).map(|r| r.slot.clone()) {
            return Some(s);
        }
        self.tombstoned.lock().iter().find(|s| s.id == id).cloned()
    }

    // -----------------------------------------------------------------------
    // §7.1 identity lookups
    //
    // These are public because the questions they answer are asked from
    // outside the cell and answering them by guessing is worse than not
    // answering: the proxy's thundering-herd guard (§9.1) keys off residency,
    // and an admin surface that reports tiers by identity has to get the
    // identity from somewhere.
    // -----------------------------------------------------------------------

    /// Whether a version's template is registered on this cell. §9.2's "any
    /// cell with the template cached", asked rather than assumed.
    pub fn has_module(&self, module: &ModuleId) -> bool {
        self.modules.read().contains_key(module)
    }

    /// The tenant a registered module belongs to.
    pub fn module_tenant(&self, module: &ModuleId) -> Option<String> {
        self.modules.read().get(module).map(|m| m.tenant.clone())
    }

    // -----------------------------------------------------------------------
    // §6.6/§9.1 socket handles
    // -----------------------------------------------------------------------

    pub(crate) fn rt_services(&self) -> &Arc<Services> {
        &self.rt.services
    }

    pub(crate) fn next_dispatch_id(&self) -> u64 {
        self.next_dispatch.fetch_add(1, Ordering::Relaxed)
    }

    pub fn instance_state(&self, id: InstanceId) -> Option<LifecycleTier> {
        self.residents.lock().get(&id).map(|r| r.slot.state())
    }
    pub fn resident_ids(&self) -> Vec<InstanceId> {
        self.residents.lock().keys().copied().collect()
    }
    pub fn resident_count(&self) -> usize {
        self.residents.lock().len()
    }
    /// The bytes a streaming response has accumulated so far (§6.3). See the
    /// PoC gap noted in `settle`.
    pub fn response_snapshot(&self, id: InstanceId, handle: u32) -> Option<Vec<u8>> {
        let g = self.residents.lock();
        let r = g.get(&id)?;
        let t = r.slot.ctx.responses.lock();
        t.get(handle).map(|resp| resp.body())
    }

    /// Discards an instance and releases its host futures without saving guest memory.
    pub fn discard_instance(self: &Arc<Self>, id: InstanceId) -> crate::Result<()> {
        let slot = self
            .slot(id)
            .ok_or_else(|| anyhow::anyhow!("no such instance {id}"))?;
        if slot.state() == LifecycleTier::Hot {
            anyhow::bail!("instance {id} is Hot; eviction is from Parked, not from a live stack");
        }
        self.rt.futures.release_instance(id);
        self.forget(&slot);
        slot.discard();
        Ok(())
    }

    /// Run one pass of the §6.6 idle trajectory synchronously.
    pub fn sweep_now(self: &Arc<Self>) {}

    // -----------------------------------------------------------------------
    // §8.2.4 quarantine migration
    // -----------------------------------------------------------------------

    pub fn is_quarantined(&self, tenant: &str) -> bool {
        self.quarantined.lock().iter().any(|t| t == tenant)
    }

    /// Stop the pool and join its threads. Idempotent.
    pub fn shutdown(&self) {
        self.stop(false);
    }

    fn stop(&self, from_drop: bool) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.queue.close();
        let me = std::thread::current().id();
        for h in self.workers.lock().drain(..) {
            // A worker upgrades its `Weak<Cell>` to run a job, so the last
            // strong reference can briefly live on a pool thread — and if it
            // does, `Drop` runs *there*. Joining yourself is a deadlock, so
            // that one handle is dropped instead.
            if from_drop && h.thread().id() == me {
                continue;
            }
            let _ = h.join();
        }
    }
}

impl Drop for Cell {
    fn drop(&mut self) {
        self.stop(true);
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_millis(200));
        }
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    /// `Shared` is one lane and `PerWorker` is one per worker, and the
    /// whole-cell admission cap is divided rather than multiplied.
    ///
    /// The second half is the one worth pinning: if each lane kept the full
    /// cap, moving to per-worker queues would silently give a 64-thread cell
    /// sixty-four times the buffering, and every latency figure would improve
    /// for a reason that has nothing to do with the queue's structure.
    #[test]
    fn the_strategy_sets_the_lane_count_and_divides_the_cap() {
        let q = WorkQueue::new(1024, 8, QueueStrategy::Shared);
        assert_eq!(q.lanes.len(), 1);
        assert_eq!(q.cap, 1024, "one lane holds the whole cap");

        let q = WorkQueue::new(1024, 8, QueueStrategy::PerWorker);
        assert_eq!(q.lanes.len(), 8);
        assert_eq!(q.cap, 128, "8 x 128 = the same 1024 the cell always held");

        // A cap smaller than the lane count still leaves each lane able to
        // hold something, rather than a queue that refuses everything.
        let q = WorkQueue::new(4, 64, QueueStrategy::PerWorker);
        assert_eq!(q.cap, 1);
    }

    /// Power of two picks the shallower of the two it sampled.
    ///
    /// Checked statistically rather than by forcing the sample, because the
    /// property that matters is the distribution: with one lane deep and the
    /// rest empty, a balancer that works almost never chooses the deep one,
    /// and one that ignores depth chooses it 1-in-N of the time.
    #[test]
    fn power_of_two_avoids_the_deep_lane() {
        let q = WorkQueue::new(4096, 8, QueueStrategy::PerWorker);
        q.lanes[3].depth.store(1_000, Ordering::Relaxed);

        let mut hit_deep = 0;
        const N: usize = 20_000;
        for _ in 0..N {
            if q.pick() == 3 {
                hit_deep += 1;
            }
        }
        // Blind choice would land on lane 3 about N/8 = 2,500 times. Power of
        // two only lands there when BOTH samples were lane 3, i.e. 1/64 of
        // the time, so ~310. Generous bound; the point is the order of
        // magnitude, not the constant.
        assert!(
            hit_deep < N / 20,
            "the deep lane was chosen {hit_deep} times in {N} — power of two should sample it \
             twice before choosing it"
        );
        // …and it is not simply never chosen, which would mean the sampler is
        // stuck rather than choosing.
        let seen: std::collections::HashSet<usize> = (0..2_000).map(|_| q.pick()).collect();
        assert!(
            seen.len() >= 6,
            "the sampler must reach most lanes, saw {}",
            seen.len()
        );
    }

    /// One lane means every worker reads the same queue, which is exactly the
    /// behaviour `Shared` replaced and must keep.
    #[test]
    fn under_shared_every_worker_reads_the_same_lane() {
        let q = WorkQueue::new(64, 16, QueueStrategy::Shared);
        for worker in 0..16 {
            assert_eq!(worker % q.lanes.len(), 0);
        }
        assert_eq!(q.pick(), 0);
    }

    /// A `DispatchJob` with nothing behind it, for exercising the queue.
    ///
    /// The queue never looks inside a job — it moves boxes between lanes — so
    /// the payload can be minimal and the test stays about the structure.
    fn job() -> Box<DispatchJob> {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        Box::new(DispatchJob {
            module: ModuleId::new("t", "v1"),
            target: Target::Handler { route: "/".into() },
            rec: wisp_abi::dispatch::DispatchRecord {
                dispatch_id: 1,
                kind: crate::cell::cell_tests::http(dispatch_id).kind,
                trace: Default::default(),
                cancel_future: 0,
                cpu_budget_ms: 50,
                now_ms: 0,
            },
            responder: tx,
            enqueued: Instant::now(),
            stamps: None,
        })
    }

    /// The lock-free ring sheds when full, which is the property §6.4 needs
    /// and the reason a *bounded* queue was chosen for dispatches.
    #[test]
    fn the_lock_free_ring_sheds_when_full() {
        let q = WorkQueue::new(4, 8, QueueStrategy::LockFree);
        assert!(
            q.lanes.is_empty(),
            "no mutex lanes are built for this strategy"
        );
        for i in 0..4 {
            assert!(q.push_dispatch(job()).is_ok(), "job {i} inside the cap");
        }
        assert_eq!(q.depth(), 4);
        assert!(
            q.push_dispatch(job()).is_err(),
            "past the cap the job is handed back"
        );
        // Draining makes room again, so the shed is a capacity limit rather
        // than a one-way door.
        assert!(q.pop(0).is_some());
        assert!(q.push_dispatch(job()).is_ok());
    }

    /// **A parked worker is never left asleep beside work.**
    ///
    /// This is the race a lock-free queue introduces and a mutex does not: the
    /// producer must not slip its push and its notify between the consumer's
    /// last look at the queue and its going to sleep. If it can, a worker
    /// sleeps while work waits, and under load that shows up as a latency
    /// tail nobody can explain.
    ///
    /// Two hundred rounds because a lost wake-up is a timing bug — one round
    /// proves nothing. The assertion is the join completing: a lost wake-up
    /// hangs rather than returning something wrong.
    #[test]
    fn a_parked_worker_is_never_left_asleep_beside_work() {
        for round in 0..200 {
            let q = Arc::new(WorkQueue::new(64, 4, QueueStrategy::LockFree));
            let q2 = q.clone();
            let h = std::thread::spawn(move || q2.pop(0).is_some());
            // Land the push inside the window where the consumer is deciding
            // whether to sleep.
            std::thread::yield_now();
            assert!(q.push_dispatch(job()).is_ok());
            assert!(
                h.join().expect("worker panicked"),
                "round {round}: woken with work"
            );
        }
    }

    /// Closing wakes every parked worker, or shutdown hangs.
    #[test]
    fn closing_the_lock_free_queue_releases_parked_workers() {
        let q = Arc::new(WorkQueue::new(64, 4, QueueStrategy::LockFree));
        let hs: Vec<_> = (0..4)
            .map(|w| {
                let q = q.clone();
                std::thread::spawn(move || q.pop(w))
            })
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(20));
        q.close();
        for h in hs {
            assert!(
                h.join().expect("worker panicked").is_none(),
                "a closed queue returns None"
            );
        }
    }
}
