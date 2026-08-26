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
use crate::lockstats::{ActiveMap, CountedMutex, CountedRwLock};

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
    ///
    /// The depth of **one** dispatch queue, and under
    /// [`QueueStructure::Shared`] and [`QueueStructure::LockFree`] — where
    /// there is exactly one — also the cell's whole admission cap. Under
    /// [`QueueStructure::PerWorker`] the per-lane depth is
    /// [`CellConfig::queue_depth_per_worker`] instead and this field is not
    /// read; [`CellConfig::admission_capacity`] is the number that is
    /// comparable across all three.
    pub admission_queue: usize,
    /// **How many queues there are** (§6.4). One decision.
    ///
    /// Defaults to [`QueueStructure::Shared`], which is what this cell has
    /// always done. See that type for what the alternatives buy and why.
    pub queue_structure: QueueStructure,
    /// **Which of them a new item goes to** — the other decision, and only
    /// meaningful when [`CellConfig::queue_structure`] builds more than one.
    ///
    /// Defaults to [`QueuePlacement::PowerOfTwo`], so
    /// `PerWorker` + the default placement is exactly the behaviour the
    /// welded `PerWorker` used to mean and prior runs stay comparable.
    pub queue_placement: QueuePlacement,
    /// The depth of **one** per-worker dispatch queue, under
    /// [`QueueStructure::PerWorker`].
    ///
    /// **Configured, not derived.** It used to be `admission_queue /
    /// exec_threads`, which meant `--exec-threads` silently changed the depth
    /// of the thing it was supposed to be varying against: 64 slots at 16
    /// threads and 16 at 64, so the exec-thread sweep moved two variables and
    /// attributed the result to one. It was also wrong at the bottom — an
    /// `admission_queue` of 4 across 64 workers gave each lane
    /// `(4/64).max(1)` = 1 slot and the cell 64, against a 4 that had been
    /// asked for.
    ///
    /// **What this pins, and what it therefore does not.** With `n` queues you
    /// can hold the depth of one queue fixed or the depth of the cell fixed,
    /// and not both: they differ by a factor of `n` by construction. This
    /// field pins the first, so the whole-cell cap becomes
    /// `queue_depth_per_worker * exec_threads` — read it from
    /// [`CellConfig::admission_capacity`] rather than assuming
    /// `admission_queue`, which is the wrong number under this structure. A
    /// comparison **between structures** therefore has to say which of the two
    /// it held constant, and the honest way to hold the cell's buffering
    /// constant across a thread sweep is to move this field with the thread
    /// count deliberately, in the record, rather than have the constructor do
    /// it silently.
    ///
    /// The default of 64 is `1024 / 16`: the per-lane share the old derivation
    /// produced for the default `admission_queue` on a sixteen-thread pool.
    /// Nothing is calibrated on it.
    pub queue_depth_per_worker: usize,
    /// **Which structure `Cell::active` is** — one mutex, or one per shard.
    ///
    /// Runtime-selectable for the same reason the queue structure is: sharding
    /// `active` is a *hypothesis* about where the execution threads lose time,
    /// and a hypothesis with no way to be run against its own baseline is an
    /// assertion. `lockstats` can show `blocked_ns` falling and cannot show
    /// where the cost went, because the coherence traffic sharding introduces
    /// is invisible to it; settling that needs the two implementations run
    /// against each other end to end, which is what this field is for.
    ///
    /// Defaults to [`crate::lockstats::ActiveMapKind::Sharded`], which is what
    /// this cell already ran, so nothing moves unless it is asked to.
    pub active_map: crate::lockstats::ActiveMapKind,
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
    /// whose timing is the measurement.
    ///
    /// **Three, counted rather than asserted.** They are `WorkQueue::
    /// stamp_queued`, closing the submit stage and opening the queue one;
    /// `run_dispatch`, closing the queue stage; and `Cell::respond`, stamping
    /// the reply. Nothing else on a request that never parks reads a clock for
    /// this mechanism — `enqueued` is read for every request, sampled or not,
    /// because `ActiveRequest::started` needs it, and `guest_ns` is the
    /// engine's own per-slice wall time. It was four until `Cell::new_stamps`
    /// stopped reading the clock into a field that `stamp_queued` overwrote
    /// before anything could read it.
    ///
    /// A request that **parks** pays two more per wake — one where the wake
    /// enters its lane and one where the execution thread takes it out — which
    /// is the price of `StageTimings::queue_ns` covering the priority lane
    /// rather than only the first admission. Both are conditional on the same
    /// sample.
    ///
    /// At the default it is one request in
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
            queue_structure: QueueStructure::default(),
            queue_placement: QueuePlacement::default(),
            queue_depth_per_worker: 64,
            active_map: crate::lockstats::ActiveMapKind::default(),
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

    /// How many dispatch queues this configuration builds.
    ///
    /// One under `Shared` and one under `LockFree` — the lock-free structure
    /// is a single shared pair of rings, not a set of them — and one per
    /// execution thread under `PerWorker`.
    pub fn queue_count(&self) -> usize {
        match self.queue_structure {
            QueueStructure::Shared | QueueStructure::LockFree => 1,
            QueueStructure::PerWorker => self.exec_threads.max(1),
        }
    }

    /// The depth of one dispatch queue, as configured.
    ///
    /// One place says which field the structure reads, so the answer cannot
    /// differ between the constructor and the report. Named for the *one*
    /// queue on purpose: [`Cell::queue_depth`] is the live count of what is
    /// waiting, which is a different question with a similar name.
    pub fn depth_per_queue(&self) -> usize {
        match self.queue_structure {
            QueueStructure::PerWorker => self.queue_depth_per_worker.max(1),
            _ => self.admission_queue.max(1),
        }
    }

    /// How many queued dispatches the cell will hold before §6.4's shed —
    /// across every queue, so the number is comparable between structures.
    pub fn admission_capacity(&self) -> usize {
        self.depth_per_queue() * self.queue_count()
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
/// **The shape: two stamps bracketing three durations, and a residual.** Not
/// four addable stages — that claim used to stand here and it was false.
/// `replied_mono_ns` is an *instant* on [`crate::mono_ns`]'s scale, not a
/// duration; it cannot be summed with anything, and the one test that checked
/// the arithmetic quietly summed three and called them "the three closed
/// stages". So, precisely:
///
/// ```text
///  admitted_mono_ns                                      replied_mono_ns
///        |                                                      |
///        |<- submit_ns ->|<- queue_ns ->|   ...   |<- guest_ns ->|
///        |<---------------- service_ns (the difference) -------->|
/// ```
///
/// * `submit_ns` and `queue_ns` are **contiguous**: one clock read closes the
///   first and opens the second (`WorkQueue::stamp_queued`), so there is no
///   gap between them and no overlap.
/// * `guest_ns` is disjoint from both — it is the engine's own per-slice wall
///   time, summed by `settle`, and the guest cannot be inside a queue.
/// * Everything else the cell did is the **residual**, and it is now
///   arithmetic rather than an admission: `service_ns - submit_ns - queue_ns -
///   guest_ns`, computable from this struct alone. That is module lookup,
///   instance acquisition, the `active` insert, `slot.enter`, `settle`, the
///   response table — real work, not measured, and not hidden.
///
/// **Not measured is not zero.** Nothing here is left at a zero initialiser
/// and then averaged in as though it were a reading: a request that was not
/// sampled carries `None` for the whole struct rather than a zeroed one, and a
/// request that never reached the guest — shed, breaker-refused, no such
/// module, reentrancy-refused — also carries `None`, because its stages would
/// be a partial decomposition of a request that has none. Every field of every
/// `Some` was written by a clock.
///
/// **The residual is deliberately not named as a stage.** Widening a stage to
/// swallow it would produce a decomposition that appears to account for
/// everything, which is the one nobody checks. Publishing it as a difference
/// keeps it visible and keeps it honest about never having been timed.
///
/// **All of it describes one request.** The sampling decision is taken once,
/// at admission, and carried on the job. Sampling each stage independently
/// would give distributions over different populations, which cannot be
/// combined and would look exactly the same in the report.
///
/// **Every figure means the same thing under every queue configuration.**
/// `submit_ns` and `queue_ns` are defined by where the stamp sits relative to
/// the *enqueue*, which is a point that exists in all three structures — see
/// `WorkQueue::stamp_queued` for what that cost and why the alternative was
/// not implementable. A column comparing structures is therefore comparing one
/// quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StageTimings {
    /// [`crate::mono_ns`] at the instant the cell accepted this request.
    ///
    /// The opening bracket, and the reason the residual is computable rather
    /// than merely acknowledged: with only `replied_mono_ns` a reader has an
    /// end and no beginning, and has to reconstruct the start from its own
    /// clock — which a report reading an archived record cannot do at all.
    ///
    /// Costs no clock read. The submit path already captures this instant for
    /// `ActiveRequest::started`; this is that same `Instant` expressed on the
    /// shared origin by [`crate::mono_ns_at`].
    pub admitted_mono_ns: u64,
    /// Entering the submit path to the instant the cell has decided where the
    /// job goes and is about to enqueue it: the breaker check, the job
    /// allocation, and — where there is more than one queue — the placement
    /// decision.
    ///
    /// **The breaker check really is inside it**, as of the fix to the struct
    /// literal that used to stamp `enqueued` after it. It takes
    /// `HangBreaker::entries`, which this cell's own `Cell::lock_stats` names
    /// as one of its seven contention suspects, so a submit stage that opened
    /// below it would have put that lock's cost in no stage at all. It is also
    /// the only lock in this stage, and it is there under every queue
    /// configuration — which is what makes the figure comparable between them.
    ///
    /// **The enqueue itself is not inside it**, and neither is what the
    /// enqueue costs: acquiring an intake lane's lock, or the ring's
    /// compare-and-swap, is charged to [`StageTimings::queue_ns`]. The text
    /// that used to stand here named the lane lock as the contended half and
    /// said it was inside — structure-specific prose on a field that also
    /// exists under `LockFree`, where no such lock exists to be inside
    /// anything. `WorkQueue::stamp_queued` carries the reasoning.
    pub submit_ns: u64,
    /// Getting into a lane and waiting there for an execution thread to pop
    /// it, summed over every slice of the dispatch.
    ///
    /// The queueing delay proper. Under [`QueueStructure::PerWorker`] it is a
    /// wait on one lane rather than on the pool, which is the whole point of
    /// that structure and the number that shows whether it worked.
    ///
    /// **The enqueue is inside it**, which is the half a reader arriving here
    /// from a report will not expect: acquiring the intake lane's lock — or
    /// the ring's compare-and-swap under [`QueueStructure::LockFree`] — and
    /// the push itself are charged here and not to
    /// [`StageTimings::submit_ns`], which stops just before them. So this is
    /// where the `Shared` structure's one contended mutex shows up, and it is
    /// deliberately the same definition under all three so that a column
    /// comparing them compares one quantity. `WorkQueue::stamp_queued` carries
    /// the reasoning and what the alternative would have cost.
    ///
    /// **Every slice, for the same reason as `guest_ns`.** A dispatch that
    /// parks goes back through a lane on its wake and waits there exactly as
    /// it did at admission — §6.3 makes that one request, and the wake lane is
    /// the *priority* lane, so how long a request waits in it is the sharpest
    /// question the two-lane design has to answer. Charging only the first
    /// wait reported the workload that queues most as the one that queues
    /// least, and left the priority lane with no measurement at all.
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

impl StageTimings {
    /// The whole interval the cell had this request, end to end.
    ///
    /// The difference between the two stamps, which is why both exist. A
    /// reader with only the three durations cannot get here: they do not add
    /// up to it, and they are not supposed to.
    pub fn service_ns(&self) -> u64 {
        self.replied_mono_ns.saturating_sub(self.admitted_mono_ns)
    }

    /// What the cell spent on this request outside the three named stages.
    ///
    /// Module lookup, instance acquisition, the `active` insert, `slot.enter`,
    /// `settle`, the response table. **Never measured** — it is this
    /// subtraction and nothing else, which is exactly why it is a method and
    /// not a field: a field would look like a reading.
    ///
    /// Saturating, and a zero is worth looking at rather than rounding past.
    /// The three stages are bounded by the bracket by construction, so a
    /// saturated zero means one of them was written from a different clock or
    /// a stamp was not taken — see [`StageTimings::is_coherent`].
    pub fn residual_ns(&self) -> u64 {
        self.service_ns()
            .saturating_sub(self.submit_ns)
            .saturating_sub(self.queue_ns)
            .saturating_sub(self.guest_ns)
    }

    /// Whether this decomposition is internally consistent: the three
    /// durations fit inside the bracket, and the bracket runs forwards.
    ///
    /// A cheap invariant a reader can apply to every record without a second
    /// source. It is what catches a unit error, two stages that overlap, and a
    /// stamp taken at the wrong end — the three failures a stage table has no
    /// other way to be wrong against.
    pub fn is_coherent(&self) -> bool {
        self.replied_mono_ns >= self.admitted_mono_ns
            && self.submit_ns + self.queue_ns + self.guest_ns <= self.service_ns()
    }
}

/// The stamps a sampled request carries while it is in flight.
///
/// Absent on every unsampled request, so the cost of the whole mechanism on
/// the common path is one `Option` discriminant and the modulo that set it.
#[derive(Debug, Clone, Copy)]
struct StageStamps {
    /// When the cell accepted the request — the submit path's `enqueued`,
    /// kept so it can be published as [`StageTimings::admitted_mono_ns`]
    /// without reading the clock again.
    ///
    /// Distinct from `queued` below, which moves: this one never does, and
    /// conflating the two is what would make the opening bracket drift to
    /// wherever the last stage boundary happened to fall.
    admitted: Instant,
    /// Where the submit stage ended and the queue stage began. Set by
    /// `WorkQueue::stamp_queued`, once, immediately before the enqueue.
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
    Wake(WakeItem),
}

/// A queued wake, and when it was queued.
///
/// The instant is here rather than derived at the pop because the interval
/// wanted is the one *before* the pop — a woken slice waits for an execution
/// thread exactly as a new dispatch does, and §6.3 makes both of them the same
/// request. See [`StageTimings::queue_ns`].
struct WakeItem {
    slot: Arc<Slot>,
    /// `Some` only for a request that [`CellConfig::stage_sample_every`]
    /// selected, so an unsampled wake costs no clock read.
    ///
    /// The producer decides, because only the producer can: the predicate is
    /// `stage_sampled(slot.ctx.dispatch_id(), …)`, which is one atomic load
    /// and one modulo and takes no lock — where asking the request itself
    /// would mean taking `Cell::active`, on the reactor thread, for telemetry.
    /// It is the same predicate `Cell::new_stamps` used at admission and on
    /// the same id, so the two cannot disagree about which request this is.
    queued: Option<Instant>,
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

thread_local! {
    /// The request this execution thread is currently running: which instance,
    /// and **which dispatch**.
    ///
    /// It exists for exactly one reader: `Cell::exec_loop`'s panic handler,
    /// which has to answer the caller of a dispatch that unwound and has no
    /// other way to learn which request that was — the job was moved into
    /// `run_dispatch` and the unwind took its stack with it. Set once the
    /// request is in `Cell::active` (which is what makes it answerable) and
    /// cleared by the worker after every job, panic or not.
    ///
    /// **The dispatch id is not decoration.** An instance id alone names a
    /// slot, and a slot stops being this thread's earlier than the job
    /// returns: `finish_and_release` removes the `active` entry and hands the
    /// instance back, so a panic in the tail *after* that point names a slot
    /// another worker may already have acquired and filled with a fresh
    /// request. Answering on the identity alone would fail that worker's
    /// caller with a `host_panic` it had nothing to do with, and delete its
    /// entry. The pair makes the handler able to ask "is the request in there
    /// still mine", which is the only question it is entitled to act on.
    static IN_FLIGHT: std::cell::Cell<Option<(InstanceId, u64)>> =
        const { std::cell::Cell::new(None) };
}

/// Shards for `Cell::active`.
///
/// Sized above the execution pool so a collision is unlikely rather than
/// certain: §6.1's default is 4x the cell's logical cpus, which is 64 on a
/// sixteen-core cell and 256 on the widest this box can build. 256 against a
/// 64-thread pool leaves a 1-in-4 chance that two threads pick the same shard.
///
/// **What that costs.** A shard is a mutex byte, an empty `HashMap` and two
/// 128-byte-padded counters, rounded up to the alignment: 384 bytes measured,
/// so 256 of them are 96 KiB per cell. The arithmetic that used to stand here
/// — "an empty `HashMap` is three words, so 256 of them cost less than one
/// instance" — was both stale and answering the wrong question; `ShardedMap::
/// new` carries the worked version and the reasoning about what the padding
/// does and does not lose.
const ACTIVE_SHARDS: usize = 256;

/// **How many work queues the cell has**, and of what kind.
///
/// This is one of the two decisions that used to be welded into a single
/// `QueueStrategy`. Removing the contention between worker threads by giving
/// each thread its own queue is this one; *which* of those queues a new item
/// is placed in is [`QueuePlacement`], and the two vary independently. The old
/// `PerWorker` meant this type's `PerWorker` **and** `PowerOfTwo` placement,
/// so that pair reproduces it exactly and prior runs stay comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum QueueStructure {
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
    /// One queue per worker. Where the new work goes is [`QueuePlacement`]'s
    /// decision, not this one.
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

impl QueueStructure {
    /// The name this structure goes by in a report and on a command line.
    ///
    /// Paired with [`QueueStructure::parse`] so a run's record round-trips:
    /// the label written into a result file is the one that reproduces it.
    pub fn label(self) -> &'static str {
        match self {
            QueueStructure::Shared => "shared",
            QueueStructure::PerWorker => "per-worker",
            QueueStructure::LockFree => "lock-free",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "shared" => QueueStructure::Shared,
            "per-worker" | "perworker" => QueueStructure::PerWorker,
            "lock-free" | "lockfree" => QueueStructure::LockFree,
            _ => return None,
        })
    }
    /// Whether [`QueuePlacement`] has anything to decide under this structure.
    ///
    /// `Shared` and `LockFree` build exactly one queue, so every placement
    /// policy picks it and the setting is inert rather than wrong. A report
    /// that prints a placement beside one of those should say so rather than
    /// imply a choice was exercised.
    pub fn placement_applies(self) -> bool {
        self == QueueStructure::PerWorker
    }
}

/// **Which queue a new item goes to**, when [`QueueStructure`] built more than
/// one of them.
///
/// The second of the two decisions. It is a pure load-balancing question and
/// has nothing to say about how the queues are built, which is why it is a
/// separate setting: all three policies below sit on top of per-worker lanes,
/// and none of them is reachable on top of a single queue because there is
/// nothing to choose between.
///
/// Every policy preserves the two-lane wake-before-dispatch priority (§6.4).
/// That is a property of what a *worker pops out of* a lane, not of which lane
/// a producer put the item in, so nothing here can affect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum QueuePlacement {
    /// Sample two lanes at random, push to the less loaded.
    ///
    /// O(1) per push, and it gets most of the balance of scanning every lane —
    /// the standard result for randomised load balancing. The default, because
    /// it is what the welded `PerWorker` always did: a run recorded before the
    /// split is a run at this policy.
    #[default]
    PowerOfTwo,
    /// Hand out lanes in turn from one shared cursor.
    ///
    /// Perfectly even *counts*, and blind to load: a lane whose worker drew a
    /// thirty-second dispatch keeps receiving its share regardless. It is here
    /// as the control — it is what "balanced" means if you do not look at what
    /// a lane is carrying — and its cursor is a single atomic that every
    /// producer increments, which is precisely the shared cacheline per-worker
    /// lanes exist to remove. Expect that to show at high thread counts; that
    /// it does is the point of having it.
    RoundRobin,
    /// One lane at random, no sampling.
    ///
    /// The cheapest placement there is, and the other control: it is what
    /// power-of-two degrades to when the load it reads is uninformative. If
    /// `PowerOfTwo` does not beat this, the counter it consults is not telling
    /// it anything and the balancer is decoration.
    Random,
}

impl QueuePlacement {
    pub fn label(self) -> &'static str {
        match self {
            QueuePlacement::PowerOfTwo => "power-of-two",
            QueuePlacement::RoundRobin => "round-robin",
            QueuePlacement::Random => "random",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "power-of-two" | "poweroftwo" | "p2c" => QueuePlacement::PowerOfTwo,
            "round-robin" | "roundrobin" | "rr" => QueuePlacement::RoundRobin,
            "random" => QueuePlacement::Random,
            _ => return None,
        })
    }
}

/// A two-lane queue, in one or many copies.
///
/// **The two lanes are the admission story, and they are unchanged by any
/// structure or placement.** Wakes jump ahead of new dispatches, and wakes are
/// never shed: overflow queues at admission and sheds *new* work, so work
/// already in flight keeps making progress and a saturated cell does not turn
/// itself into a pile of half-finished requests (§6.4).
///
/// `lanes` is one entry under [`QueueStructure::Shared`] — in which case this
/// is exactly the structure it replaces — and one per worker under
/// [`QueueStructure::PerWorker`].
struct WorkQueue {
    /// Empty under [`QueueStructure::LockFree`].
    lanes: Box<[Lane]>,
    /// `Some` only under [`QueueStructure::LockFree`].
    free: Option<LockFreeLanes>,
    /// The depth of one dispatch lane, as configured.
    ///
    /// **Not derived from the lane count.** It used to be the whole-cell cap
    /// divided across the lanes, which made the doc that stood here — "the
    /// strategy changes where work waits and not how much of it the cell will
    /// hold" — false in both directions: the depth of a lane moved with
    /// `exec_threads`, and `(cap / n).max(1)` overshot the whole-cell total
    /// whenever the division rounded to zero. It is now what
    /// [`CellConfig::depth_per_queue`] says, the cell's total is
    /// [`CellConfig::admission_capacity`], and neither is a function of the
    /// other.
    cap: usize,
    structure: QueueStructure,
    placement: QueuePlacement,
    /// [`QueuePlacement::RoundRobin`]'s cursor, and nothing else's.
    cursor: AtomicUsize,
}

/// One lane: the two queues, the lock over them, and a load counter that can
/// be read without taking it.
///
/// `load` is padded to its own cache line. Without that, sampling two lanes'
/// loads would pull in a line that a third lane's push is writing, and the
/// balancer would generate exactly the sharing it exists to avoid.
#[repr(align(128))]
struct Lane {
    inner: CountedMutex<QueueInner>,
    cv: Condvar,
    /// **What this lane still owes:** everything queued in it, of either
    /// kind, plus everything its worker has taken out and not yet finished.
    ///
    /// It is not the dispatch queue's length, which is what it used to be, and
    /// that is the whole point. As a length it could not see the two states
    /// that make a lane the worst possible choice — a lane holding four
    /// hundred queued wakes read zero, because `push_wake` never touched it,
    /// and a lane whose worker was twenty seconds into a thirty-second
    /// dispatch read zero, because the pop had already decremented it. Both
    /// were maximally attractive to a placer that reads this to avoid load.
    ///
    /// So: incremented by every push, of either lane, and decremented by the
    /// worker when the job is *done* rather than when it is taken. Approximate
    /// by design — it is read outside the lock, and a stale read costs a
    /// slightly worse choice rather than a wrong one — and O(1) and lock-free
    /// to read, which is what lets a producer sample two of them per push.
    load: AtomicUsize,
}

/// The lock-free pair, plus the parking spot a worker sleeps on.
struct LockFreeLanes {
    /// Bounded: `push` returns the job back when full, which is the shed.
    dispatches: crossbeam_queue::ArrayQueue<Box<DispatchJob>>,
    /// Unbounded: §6.4 never sheds a wake.
    wakes: crossbeam_queue::SegQueue<WakeItem>,
    closed: AtomicBool,
    /// How many producers are between their `closed` check and their push.
    ///
    /// **This is what makes the lock-free path as safe as the mutex one.** The
    /// mutex path mutates `closed` and the queue under the same lock, so a
    /// dispatch cannot land after the queue is closed. Here they are two
    /// operations, and a stronger memory ordering does not join them: a
    /// producer can read `closed` as false, be descheduled, and push into a
    /// ring every worker has already drained and left — a job that is neither
    /// shed nor served, and a caller that is never answered.
    ///
    /// A producer announces itself here *before* reading `closed`, so a closer
    /// that has already stored `closed` and then reads zero knows there is no
    /// producer left that could still push. `close` waits for that zero and
    /// then drains, which turns "either shed or served" into a property of the
    /// pair rather than of a lucky interleaving. Both are `SeqCst`, and they
    /// have to be: the argument is about the order of operations on *two*
    /// locations, which is the one thing only a total order gives.
    pushing: AtomicUsize,
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
    wakes: std::collections::VecDeque<WakeItem>,
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
    /// `depth` is the depth of **one** dispatch queue, already decided by
    /// [`CellConfig::depth_per_queue`]. Nothing here divides it by anything: the
    /// lane count and the lane depth are two independent configured
    /// quantities, which is what stops an `--exec-threads` sweep from moving
    /// the queue depth underneath itself.
    fn new(
        depth: usize,
        workers: usize,
        structure: QueueStructure,
        placement: QueuePlacement,
    ) -> Self {
        let cap = depth.max(1);
        let n = match structure {
            QueueStructure::Shared => 1,
            QueueStructure::PerWorker => workers.max(1),
            QueueStructure::LockFree => 0,
        };
        let free = (structure == QueueStructure::LockFree).then(|| LockFreeLanes {
            dispatches: crossbeam_queue::ArrayQueue::new(cap),
            wakes: crossbeam_queue::SegQueue::new(),
            closed: AtomicBool::new(false),
            pushing: AtomicUsize::new(0),
            park: Mutex::new(()),
            cv: Condvar::new(),
            parked: AtomicUsize::new(0),
        });
        Self {
            lanes: (0..n)
                .map(|_| Lane {
                    inner: CountedMutex::new(QueueInner::default()),
                    cv: Condvar::new(),
                    load: AtomicUsize::new(0),
                })
                .collect(),
            free,
            cap,
            structure,
            placement,
            cursor: AtomicUsize::new(0),
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

    /// The lane a new item should go to — [`QueuePlacement`] applied.
    ///
    /// With one lane there is nothing to decide, and that early return is what
    /// makes the placement setting inert rather than wrong under `Shared`.
    #[inline]
    fn pick(&self) -> usize {
        let n = self.lanes.len();
        if n <= 1 {
            return 0;
        }
        match self.placement {
            QueuePlacement::PowerOfTwo => {
                let a = lane_rand(n);
                let b = lane_rand(n);
                if self.lanes[a].load.load(Ordering::Relaxed)
                    <= self.lanes[b].load.load(Ordering::Relaxed)
                {
                    a
                } else {
                    b
                }
            }
            // `Relaxed`: nothing is ordered against this and a torn view of
            // the sequence costs an uneven turn, not a wrong lane.
            QueuePlacement::RoundRobin => self.cursor.fetch_add(1, Ordering::Relaxed) % n,
            QueuePlacement::Random => lane_rand(n),
        }
    }

    /// One job this lane's worker had taken is finished.
    ///
    /// Paired with the push that incremented it, and called by the worker
    /// after the job has run rather than by `pop` — see [`Lane::load`] for why
    /// the interval has to include the execution and not stop at the pop.
    /// Under `LockFree` there are no lanes and nothing to decrement.
    #[inline]
    fn job_done(&self, worker: usize) {
        if self.lanes.is_empty() {
            return;
        }
        let lane = &self.lanes[worker % self.lanes.len()];
        // `fetch_update` rather than `fetch_sub`: the counter is `usize` and a
        // decrement that ever ran without its push would wrap to a value that
        // makes the lane look infinitely loaded forever. Saturating here keeps
        // a bookkeeping slip a bookkeeping slip.
        let _ = lane
            .load
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    /// Close the submit stage and open the queue stage, on a sampled job.
    ///
    /// One clock read produces both: the instant the two stages meet is the
    /// end of one interval and the start of the next, and reading it twice
    /// would leave a gap between them that belongs to neither.
    ///
    /// **The boundary is: placement decided, enqueue not yet begun.** Called
    /// at that point in *every* structure, which is the whole reason it is
    /// where it is.
    ///
    /// It used to sit after the lane lock was acquired on the mutex path and
    /// before the ring's compare-and-swap on the lock-free one — so `Shared`
    /// and `PerWorker` charged the contended lock acquisition to `submit_ns`
    /// and `LockFree` charged its contended CAS to nobody. A report column
    /// comparing `submit` across structures was comparing two different
    /// quantities, and it flattered `LockFree` at precisely the stage meant to
    /// show what the lock costs.
    ///
    /// **Why the boundary went before the enqueue rather than after it.**
    /// After it is not implementable: the enqueue moves the job out of the
    /// producer's hands — into a `VecDeque` behind a lock, or into a ring a
    /// consumer may pop before the next instruction — so there is no job left
    /// to stamp. Before it is implementable identically in both, so that is
    /// the boundary both can share.
    ///
    /// **Where the enqueue's cost goes, then: into `queue_ns`.** Not into a
    /// gap — the two stages still meet at one instant, with nothing between
    /// them. And it belongs there on the merits: from the request's point of
    /// view, time spent waiting for the intake lock is time it is not being
    /// served, which is what queueing is. The lock's cost has not gone
    /// missing, it has moved to the stage that is defined the same way under
    /// every structure — and `Cell::lock_stats` measures the same lock a
    /// second, independent way.
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
            // There is no placement to make here — one ring — so the boundary
            // is simply "before the enqueue", above the `pushing` bookkeeping
            // as well as above the compare-and-swap. See `stamp_queued`.
            Self::stamp_queued(&mut j);
            // Announce before the check, release after the push. See
            // `LockFreeLanes::pushing`: without this pair the check and the
            // push are two steps and a job can land in a ring that every
            // worker has already left.
            f.pushing.fetch_add(1, Ordering::SeqCst);
            if f.closed.load(Ordering::SeqCst) {
                f.pushing.fetch_sub(1, Ordering::SeqCst);
                return Err(j);
            }
            // A full ring hands the job straight back, which is exactly the
            // shed the mutex version does at `dispatches.len() >= cap`.
            let pushed = f.dispatches.push(j);
            f.pushing.fetch_sub(1, Ordering::SeqCst);
            pushed?;
            Self::free_notify(f);
            return Ok(());
        }
        // Placement is submit work, so `pick` is inside the submit stage and
        // the lane lock below is not. That pairs with the lock-free branch
        // above, which stamps at the same point in its own sequence.
        let lane = &self.lanes[self.pick()];
        Self::stamp_queued(&mut j);
        let mut g = lane.inner.lock();
        if g.closed || g.dispatches.len() >= self.cap {
            return Err(j);
        }
        g.dispatches.push_back(j);
        lane.load.fetch_add(1, Ordering::Relaxed);
        drop(g);
        lane.cv.notify_one();
        Ok(())
    }

    /// A wake is never shed, so it is placed regardless of capacity.
    ///
    /// It counts toward the lane's load all the same: a lane holding four
    /// hundred wakes is the busiest lane in the cell, and a placer that could
    /// not see them was choosing it on purpose.
    fn push_wake(&self, item: WakeItem) {
        if let Some(f) = &self.free {
            f.pushing.fetch_add(1, Ordering::SeqCst);
            if f.closed.load(Ordering::SeqCst) {
                f.pushing.fetch_sub(1, Ordering::SeqCst);
                return;
            }
            f.wakes.push(item);
            f.pushing.fetch_sub(1, Ordering::SeqCst);
            Self::free_notify(f);
            return;
        }
        let lane = &self.lanes[self.pick()];
        let mut g = lane.inner.lock();
        if g.closed {
            return;
        }
        g.wakes.push_back(item);
        lane.load.fetch_add(1, Ordering::Relaxed);
        drop(g);
        lane.cv.notify_one();
    }

    /// Take the next job for `worker`.
    ///
    /// Under [`QueueStructure::Shared`] every worker reads lane zero, which is
    /// the original behaviour exactly. Under [`QueueStructure::PerWorker`] a
    /// worker reads only its own lane: it blocks there rather than stealing,
    /// because placement is what keeps the lanes even, and a stealing scan
    /// would put every idle worker back on every other worker's lock — the
    /// sharing per-worker lanes exist to remove.
    ///
    /// **Wakes before dispatches, in every structure.** That is §6.4's
    /// admission story and it is a property of this function, not of where a
    /// producer put the item, which is why no placement policy can weaken it.
    ///
    /// The lane's load is deliberately *not* decremented here; the worker
    /// releases it in [`WorkQueue::job_done`] once the job has actually run.
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
        // `lock_bare`, and only here: `parking_lot::Condvar::wait` takes a
        // `parking_lot::MutexGuard` and nothing else, so this one call site
        // needs the bare guard. Splitting it out is what lets `lock` become a
        // release-then-record wrapper for every *other* caller — the ten
        // `Cell::active` sites, `durables`, `residents`, the breaker, the
        // future table — and lifts the in-hold `fetch_add` off all of them.
        // "Bare" is about the guard, not the accounting: this is instrumented
        // exactly as `lock` is, and `WorkQueue::inner` is one of the two locks
        // the whole exercise is about, so it has to be.
        let lane = &self.lanes[worker % self.lanes.len()];
        let mut g = lane.inner.lock_bare();
        loop {
            if let Some(s) = g.wakes.pop_front() {
                return Some(Job::Wake(s));
            }
            if let Some(d) = g.dispatches.pop_front() {
                return Some(Job::Dispatch(d));
            }
            if g.closed {
                return None;
            }
            lane.cv.wait(&mut g);
        }
    }

    /// Stop accepting work, and answer whatever the close raced.
    ///
    /// **The invariant is that a dispatch is either shed or served, never
    /// stranded** — a caller that got `true` from `submit_with` is waiting on
    /// its channel and has no other way to learn the cell went away.
    ///
    /// The mutex path gets that for free: `closed` and the queue are mutated
    /// under one lock, so a dispatch cannot be admitted after the close, and
    /// `pop` drains both lanes before it reports the closure, so a dispatch
    /// admitted before it is still served. The lock-free path gets it here —
    /// see [`LockFreeLanes::pushing`] for the race and why an ordering change
    /// alone does not close it. The wait is for at most one ring push and no
    /// syscall.
    fn close(&self) {
        if let Some(f) = &self.free {
            f.closed.store(true, Ordering::SeqCst);
            {
                let _g = f.park.lock();
                f.cv.notify_all();
            }
            while f.pushing.load(Ordering::SeqCst) > 0 {
                std::thread::yield_now();
            }
            // Whatever is left was admitted and will never be popped, because
            // the workers are on their way out. Answer it rather than let the
            // caller wait for a reply nobody is going to send.
            while let Some(j) = f.dispatches.pop() {
                let _ = j.responder.send(DispatchResult::shed(j.rec.dispatch_id));
            }
            // A wake has no responder — the request it belongs to is answered
            // through `Cell::active`, which is going away with the cell.
            // Dropping them here at least releases the slots.
            while f.wakes.pop().is_some() {}
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
    /// the unsharded locks beside it however many lanes there are — or `None`
    /// when there is no such lock at all.
    ///
    /// **`None` is the point.** Under `LockFree` this queue has no data lock:
    /// the parking mutex guards nothing and is taken only to sleep. Reporting
    /// that as a zero sample made it indistinguishable from "measured, and
    /// nothing blocked", and a lock table reading every counter as zero would
    /// then print something like *"whatever these threads were blocking on, it
    /// was not one of the locks instrumented here"* — which is false, because
    /// the lock it is talking about does not exist to be instrumented. So the
    /// row is omitted rather than zeroed, and the absence carries the fact.
    fn lock_stats(&self) -> Option<crate::lockstats::LockSample> {
        if self.free.is_some() {
            return None;
        }
        Some(
            self.lanes
                .iter()
                .fold(crate::lockstats::LockSample::default(), |acc, l| {
                    let x = l.inner.stats().snapshot();
                    crate::lockstats::LockSample {
                        contended: acc.contended + x.contended,
                        blocked_ns: acc.blocked_ns + x.blocked_ns,
                    }
                }),
        )
    }

    fn structure(&self) -> QueueStructure {
        self.structure
    }

    fn placement(&self) -> QueuePlacement {
        self.placement
    }

    /// The name this queue's contention row goes by in [`Cell::lock_stats`].
    ///
    /// The structure is in the name because the row means different things
    /// under each: 365 s of blocked time under `shared` is one mutex every
    /// thread queued on, the same figure under `per-worker` is that time
    /// spread over `exec_threads` locks, and a zero under `lock-free` is "no
    /// lock to block on" rather than "nothing blocked". A reader comparing two
    /// runs' tables needs to know which of the three produced the row, and the
    /// row is the only place the answer travels with the number.
    fn row_name(&self) -> &'static str {
        match self.structure {
            QueueStructure::Shared => "WorkQueue::inner[shared]",
            QueueStructure::PerWorker => "WorkQueue::inner[per-worker]",
            // Never emitted: `lock_stats` returns `None` under this structure
            // and the row does not appear at all. Named anyway, so that a
            // caller which does ask cannot be handed a misleading label.
            QueueStructure::LockFree => "WorkQueue::inner[lock-free]",
        }
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
    /// A copy of [`CellConfig::stage_sample_every`], so a reactor thread can
    /// decide whether this wake's lane wait is worth a clock read without
    /// reaching back into the cell for the answer.
    stage_sample_every: u64,
}

impl WakeSink for CellWake {
    fn signal(&self, owner: InstanceId) {
        let slot = self.index.lock().get(&owner).and_then(Weak::upgrade);
        if let Some(s) = slot {
            self.metrics.wakes.fetch_add(1, Ordering::Relaxed);
            // The sampling decision, taken from the instance's own dispatch id
            // — an atomic load and a modulo, on a reactor thread, with no lock
            // taken. See `WakeItem::queued`.
            let queued =
                stage_sampled(s.ctx.dispatch_id(), self.stage_sample_every).then(Instant::now);
            self.queue.push_wake(WakeItem { slot: s, queued });
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
    active: crate::lockstats::ActiveMapImpl<InstanceId, ActiveRequest>,
    breaker: HangBreaker,
    metrics: Arc<Metrics>,
    hot: AtomicUsize,
    next_dispatch: AtomicU64,
    /// Tenants that have been unilaterally moved to a quarantine cell (§8.2.4)
    /// and may no longer be admitted here.
    quarantined: Mutex<Vec<String>>,
    shutdown: Arc<AtomicBool>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// Host panics caught at a thread boundary and contained. See
    /// [`Cell::caught_panics`].
    caught_panics: AtomicU64,
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
        // Pin the shared monotonic origin before this cell can take a
        // request, so `StageTimings::admitted_mono_ns` is never the saturated
        // zero `mono_ns_at` gives for an instant older than the origin.
        let _ = crate::mono_ns();
        let queue = Arc::new(WorkQueue::new(
            cfg.depth_per_queue(),
            cfg.exec_threads,
            cfg.queue_structure,
            cfg.queue_placement,
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
                stage_sample_every: cfg.stage_sample_every,
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
            active: crate::lockstats::ActiveMapImpl::new(cfg.active_map, ACTIVE_SHARDS),
            metrics,
            hot: AtomicUsize::new(0),
            next_dispatch: AtomicU64::new(1),
            quarantined: Mutex::new(Vec::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            workers: Mutex::new(Vec::new()),
            caught_panics: AtomicU64::new(0),
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
                    // One sweep's worth of blast radius. §6.6's idle
                    // trajectory runs "with no tenant involvement", which is
                    // exactly what makes a panic here so expensive: nothing
                    // restarts this thread, and the loss is silent — captures
                    // simply stop happening and every DI in the cell stays
                    // resident for as long as the process lives.
                    let swept = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        cell.sweep_idle()
                    }));
                    if swept.is_err() {
                        cell.note_caught_panic("sweep");
                    }
                }
            })
            .expect("idle sweeper");
        self.workers.lock().push(h);
    }

    /// One execution thread: take a job, run it, release the lane, repeat.
    ///
    /// **The job body is caught.** A host assertion inside a dispatch — §6.4's
    /// tombstone assertion is the one that has actually fired — used to take
    /// the whole thread with it, and nothing brings an execution thread back.
    /// A cell that loses one silently gets `1/exec_threads` slower and keeps
    /// answering; a cell that loses the last one stops answering with no
    /// record of why. Catching it turns that into one failed request, one
    /// counter, and a log line naming the panic.
    ///
    /// `AssertUnwindSafe` is honest here rather than a way past the compiler.
    /// What it promises is that no *observer* is left holding a half-updated
    /// invariant, and a panic mid-dispatch already means the instance is being
    /// discarded: the `CallGuard` unwinds and returns it, and any request
    /// state that was mid-write belongs to a request that is now failed.
    ///
    /// **Three residues, all real.** Named rather than waved at, because the
    /// point of catching a panic is not to make it look like nothing happened:
    ///
    /// 1. The `hot` gauge keeps its increment. Nothing gates admission on it —
    ///    `CellConfig::max_hot` is configured and never enforced — so this is
    ///    telemetry drift and not a wedge.
    /// 2. **The instance leaks out of its pool.** `finish_and_release` never
    ///    ran, so the slot is neither released, reset, nor discarded. It stays
    ///    in `residents` and keeps showing in the residency gauges, which is
    ///    the right way round: a leak that is visible is a leak somebody
    ///    fixes.
    /// 3. **So do that dispatch's futures**, including the cancellation future
    ///    `run_dispatch` allocates before entering the guest. `FutureTable::
    ///    end_dispatch` and `release_instance` are called from
    ///    `finish_and_release` and nowhere else, so an unwind past it leaves
    ///    the table entries behind. Deliberately not half-fixed here: reaping
    ///    this dispatch's cancellation future while the instance and the rest
    ///    of its futures leak anyway would be a partial cleanup that reads as
    ///    a complete one, and residue 2 is where the fix belongs.
    ///
    /// The `active` entry is *not* among them — `answer_after_panic` removes
    /// it, along with answering the caller.
    fn exec_loop(me: Weak<Self>, queue: Arc<WorkQueue>, worker: usize) {
        while let Some(job) = queue.pop(worker) {
            let cell = me.upgrade();
            if let Some(cell) = &cell {
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
                    Job::Dispatch(j) => cell.run_dispatch(*j),
                    Job::Wake(w) => cell.run_wake(w.slot, w.queued),
                }));
                // Cleared either way, so a job that finished normally cannot
                // leave a stale id for the next panic to answer.
                let holding = IN_FLIGHT.replace(None);
                if ran.is_err() {
                    cell.note_caught_panic("dispatch");
                    cell.answer_after_panic(holding);
                }
            }
            // Outside the `if`, and after the job either way: the lane's load
            // counts what it has handed out, so it has to be released whether
            // the job ran, panicked, or was dropped because the cell went
            // away. A push that is never released makes its lane look
            // permanently busy to every future placement decision.
            queue.job_done(worker);
            if cell.is_none() {
                return;
            }
        }
    }

    /// Tell the caller of a panicked dispatch that it panicked.
    ///
    /// Without this the request does not fail so much as stop: its responder
    /// is not dropped by the unwind — it was moved into `Cell::active` before
    /// the guest was entered — so `dispatch_blocking` waits out its whole
    /// timeout, and `submit_with`, which shares one channel across every
    /// request in flight, never learns anything at all. A caller that was told
    /// `true` at admission is owed an answer, and 500 with a named cause is
    /// the answer.
    ///
    /// **It answers only if the request in there is still the one this thread
    /// was running.** The instance id alone is not enough to establish that
    /// and using it alone was a defect: `finish_and_release` removes the
    /// `active` entry and returns the instance to its pool *before* the job
    /// returns, so a panic in the tail after that point names a slot another
    /// worker may already hold. On the identity alone this would fail that
    /// worker's caller with a `host_panic` that had nothing to do with the
    /// fault, and delete its entry — a rare window and a narrow one, but it
    /// turns one broken request into two, and the second one was healthy.
    /// `ActiveRequest::dispatch_id` settles it, checked inside the same
    /// acquisition that takes the responder.
    ///
    /// A mismatch — or no entry at all — is not an error and not counted: it
    /// means the request this thread was running had already been cleaned up
    /// before the panic, so there is nothing owed to anybody.
    ///
    /// What it deliberately does **not** do is touch the slot's lifecycle. The
    /// unwind has already put the instance back through `CallGuard::drop`, and
    /// what state a host panic left behind is exactly what nothing here knows
    /// — so the instance is neither reset nor pooled nor tombstoned. See
    /// `Cell::exec_loop` for what that leaks.
    fn answer_after_panic(&self, holding: Option<(InstanceId, u64)>) {
        let Some((id, dispatch_id)) = holding else {
            return;
        };
        // The identity check and the take are one acquisition. The two `false`
        // cases need the same ending and the two halves of the `true` one do
        // not: `(true, None)` is our entry already answered — a panic after
        // `respond` and before the job returned — and still needs removing.
        // Only the lock can tell any of them apart.
        let (mine, tx) = self
            .active
            .with_mut(&id, |a| {
                if a.dispatch_id == dispatch_id {
                    (true, a.responder.take())
                } else {
                    (false, None)
                }
            })
            .unwrap_or((false, None));
        if !mine {
            return;
        }
        if let Some(tx) = tx {
            let _ = tx.send(DispatchResult {
                status: 500,
                body: b"host fault: a panic was caught inside this dispatch".to_vec(),
                trap: Some("host_panic".into()),
                instance: id,
                ..DispatchResult::shed(dispatch_id)
            });
        }
        // Ours a moment ago, and it cannot have become anyone else's since: a
        // new request for this instance would have to insert over an entry
        // that is still here, and the single-writer check refuses that.
        self.active.remove(&id);
    }

    /// A panic caught at a thread boundary, counted and named.
    fn note_caught_panic(&self, stage: &'static str) {
        self.caught_panics.fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            cell = %self.cfg.name, stage,
            "a host panic was caught at the thread boundary and contained; the thread \
             survives, a dispatch is failed with a 500 and a sweep is simply skipped. This \
             is a HOST bug — a guest fault is a trap, not a panic — and the count is \
             `Cell::caught_panics`"
        );
    }

    /// How many host panics this cell has caught and contained.
    ///
    /// **Not zero is a host bug**, always: a guest fault arrives as a trap and
    /// never as a panic. It lives here rather than on [`Metrics`] because
    /// `Metrics` is §10's tenant-facing surface and this is not a fact about
    /// any tenant; it is a fact about this build.
    pub fn caught_panics(&self) -> u64 {
        self.caught_panics.load(Ordering::Relaxed)
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

    /// The queue structure this cell is **actually running**, read from the
    /// queue rather than from the config.
    ///
    /// The distinction is the point: `lock_stats` reports one row for the work
    /// queue and that row means three different things under the three
    /// structures, so a run's record has to carry which one produced it. The
    /// queue is the thing that was built, so the queue is asked.
    pub fn queue_structure(&self) -> QueueStructure {
        self.queue.structure()
    }

    /// The placement policy this cell is running.
    ///
    /// Meaningless unless [`QueueStructure::placement_applies`] — under one
    /// queue every policy picks it, so a report should say "n/a" rather than
    /// print a policy that was never exercised.
    pub fn queue_placement(&self) -> QueuePlacement {
        self.queue.placement()
    }

    /// The depth of one dispatch queue, and how many of them there are.
    ///
    /// Published as a pair because neither half is meaningful alone: the
    /// product is what the cell will hold before §6.4's shed, and it is
    /// `admission_queue` only under a single-queue structure.
    pub fn queue_shape(&self) -> (usize, usize) {
        (self.cfg.depth_per_queue(), self.cfg.queue_count())
    }

    /// How many queued dispatches this cell holds before it sheds, across
    /// every queue. See [`CellConfig::admission_capacity`].
    pub fn admission_capacity(&self) -> usize {
        self.cfg.admission_capacity()
    }

    /// Which structure `Cell::active` is, read from the map rather than from
    /// the config — the same reason [`Cell::queue_structure`] asks the queue.
    pub fn active_map(&self) -> crate::lockstats::ActiveMapKind {
        self.active.kind()
    }

    /// The sampling decision, taken once per request at admission.
    ///
    /// **Once, and carried**, rather than re-derived at each of the four
    /// stamping points. Re-deriving would be three more integer divisions per
    /// request on the dispatch path, and — the part that actually matters —
    /// would let the four stages disagree about which requests they describe
    /// if the configuration were ever changed mid-run.
    /// `enqueued` is the caller's own admission stamp, reused rather than
    /// re-read. It used to be `Instant::now()` here, which was a clock read on
    /// the sampled path whose value was overwritten by `stamp_queued` before
    /// anything could observe it — dead, and the fourth of the "three extra
    /// `clock_gettime` calls per request" the field's own doc promises. Taking
    /// the caller's instant costs nothing and, if `stamp_queued` were ever
    /// skipped, degrades to a `queue_ns` that includes the submit rather than
    /// to a reading off an unrelated origin.
    #[inline]
    fn new_stamps(&self, dispatch_id: u64, enqueued: Instant) -> Option<StageStamps> {
        // `then_some`, not `then`: with the clock read gone there is nothing
        // left to defer — the value is three words copied from arguments the
        // caller already has.
        stage_sampled(dispatch_id, self.cfg.stage_sample_every).then_some(StageStamps {
            admitted: enqueued,
            queued: enqueued,
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
    ///
    /// **Two rules govern the rows, and they are rules rather than a list of
    /// exceptions**, so a reader or a report writes one piece of logic for
    /// both:
    ///
    /// 1. *A row name may carry a `[configuration]` suffix* — as in
    ///    `WorkQueue::inner[per-worker]` — naming which form of a
    ///    runtime-selectable structure produced it. The same figure means
    ///    different things under different forms: 365 s of blocked time on one
    ///    shared mutex is not 365 s spread across `exec_threads` separate
    ///    ones, and a table being compared with another run's needs to know
    ///    which it is looking at.
    /// 2. *A lock that does not exist under the current configuration produces
    ///    no row at all.* Never a zero — a zero is a measurement, and "there
    ///    was nothing here to measure" is not one. A reader that finds every
    ///    row zero may say the blocking was elsewhere; a reader that finds a
    ///    row missing knows why it is missing.
    pub fn lock_stats(&self) -> Vec<(&'static str, crate::lockstats::LockSample)> {
        // Rule 1 applies here for the same reason it applies to the queue:
        // this row is one mutex under `Plain` and 256 shards summed under
        // `Sharded`, and a table read against another run's has to know which.
        let active_row = match self.active.kind() {
            crate::lockstats::ActiveMapKind::Plain => "Cell::active[plain]",
            crate::lockstats::ActiveMapKind::Sharded => "Cell::active[sharded]",
        };
        let mut rows = vec![
            (active_row, self.active.lock_stats()),
            ("FutureTable::inner", self.rt.futures.lock_stats()),
        ];
        // Omitted entirely under a structure that has no such lock — rule 2
        // above. See `WorkQueue::lock_stats`.
        if let Some(q) = self.queue.lock_stats() {
            rows.push((self.queue.row_name(), q));
        }
        rows.extend([
            ("HangBreaker::entries", self.breaker.lock_stats()),
            ("Cell::residents", self.residents.stats().snapshot()),
            ("Cell::modules", self.modules.stats().snapshot()),
        ]);
        rows
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

    /// How much work is **waiting** right now, across every queue.
    ///
    /// A live count of queued items, not a capacity: the configured depth of
    /// one queue is [`CellConfig::depth_per_queue`] and the cell's whole
    /// admission cap is [`Cell::admission_capacity`].
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
        // The submit stage opens here, above the breaker check. See
        // `Cell::submit_with` for why that check is inside it.
        let enqueued = Instant::now();
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
            enqueued,
            stamps: self.new_stamps(dispatch_id, enqueued),
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
        // **The submit stage opens above the breaker check**, which is where
        // [`StageTimings::submit_ns`] has always said it opens and where the
        // struct literal below — evaluated after the check, since that is a
        // statement and this is an expression — quietly did not.
        //
        // Resolved in favour of the doc rather than the code, because the
        // check takes `HangBreaker::entries`, one of the seven locks this
        // cell's own `lock_stats` names as its contention suspects. A submit
        // stage that started underneath it would leave that lock's cost
        // outside every named stage and inside the residual the caller
        // computes — which is exactly where a contended lock goes unnoticed,
        // and the reason the stage breakdown exists at all.
        //
        // It also moves `ActiveRequest::started`, and therefore
        // `Metrics::request_us`, up by the same interval. That is the more
        // honest reading of "how long the cell had this request" too.
        let enqueued = Instant::now();
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
            enqueued,
            stamps: self.new_stamps(dispatch_id, enqueued),
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
        // This slice's queue wait closes here, at the top, before the module
        // lookup: everything below this line is work the cell does *for* the
        // request, and folding the first `modules` read into the queueing
        // delay would charge a lock the request needed to the lane it was
        // waiting in.
        //
        // `+=`, not `=`, and the same reasoning as `guest_ns`: §6.3 makes one
        // request a lifecycle spanning many slices, and a woken slice queues
        // for a worker exactly as this one did. `run_wake` adds its own wait
        // to the same field. A `queue_ns` that stopped here would report the
        // workload that queues most — the one that parks and is woken — as the
        // one that queues least.
        let mut job = job;
        if let Some(s) = &mut job.stamps {
            s.queue_ns += s.queued.elapsed().as_nanos() as u64;
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
        if self.active.contains_key(&slot.id) {
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

        self.active.insert(
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

        // From here the request is answerable, so from here a panic on this
        // thread can answer it. See `Cell::answer_after_panic`.
        IN_FLIGHT.set(Some((slot.id, job.rec.dispatch_id)));

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
                    self.active.remove(&slot.id);
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
            self.queue_wake(slot);
        }
    }

    /// Put an instance in the wake lane, stamped if its request is one of the
    /// sampled ones.
    ///
    /// One place applies the predicate for every producer inside the cell, so
    /// the three push sites cannot come to differ about which requests carry a
    /// queue stamp. `CellWake::signal` is the fourth and cannot use this — it
    /// holds no cell — so it carries the same two lines against the same id.
    fn queue_wake(&self, slot: Arc<Slot>) {
        let queued =
            stage_sampled(slot.ctx.dispatch_id(), self.cfg.stage_sample_every).then(Instant::now);
        self.queue.push_wake(WakeItem { slot, queued });
    }

    /// One woken slice, and every further slice the instance can take without
    /// going back through a lane.
    ///
    /// `queued` is when this wake entered its lane, on the requests that are
    /// sampled. It is charged to [`StageTimings::queue_ns`], which is the
    /// per-request sum across slices; the loop below does not re-queue, so the
    /// stamp is spent once and the later slices in this call queued for
    /// nothing and are charged nothing.
    fn run_wake(self: &Arc<Self>, slot: Arc<Slot>, queued: Option<Instant>) {
        // This slice's lane wait closes **here**, at the top, before the
        // `residents` lock — exactly where `run_dispatch` closes its own, and
        // for the same reason: everything below this line is work the cell
        // does *for* the request.
        //
        // Read now and written later. The write needs `Cell::active`, which
        // this function already locks once further down for the budget; taking
        // it here as well would be telemetry paying for telemetry on the lock
        // the cell's own `lock_stats` puts at the top of its list. So the
        // *measurement* boundary is this line and the *store* is deferred,
        // which is the only way to have both.
        let mut waited = queued.map(|t| t.elapsed().as_nanos() as u64);
        // A wake runs an existing request, which is therefore already in
        // `Cell::active` and already answerable. The id comes from the
        // instance's own context, which `begin_dispatch` set and every slice
        // of this request carries. See `Cell::answer_after_panic`.
        IN_FLIGHT.set(Some((slot.id, slot.ctx.dispatch_id())));
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
            //
            // The lane wait measured at the top of this function is stored in
            // the same acquisition. `waited` is taken, so a loop that runs
            // several slices charges the one wait it actually had: the later
            // slices did not go back through a lane and are charged nothing.
            let budget = self
                .active
                .with_mut(&slot.id, |a| {
                    if let (Some(ns), Some(s)) = (waited.take(), a.stamps.as_mut()) {
                        s.queue_ns += ns;
                    }
                    a.budget.after(a.cpu_ns)
                })
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

        let (lifecycle, module, route) = self
            .active
            .with_mut(&slot.id, |req| {
                req.cpu_ns += out.cpu_ns;
                req.wall_ns += out.wall_ns;
                req.slices += 1;
                (req.lifecycle, req.module.clone(), req.route.clone())
            })
            .unwrap_or_else(|| {
                (
                    entry.manifest.handlers.lifecycle,
                    entry.activator.module().clone(),
                    String::new(),
                )
            });
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
        let (dispatch_id, cpu_ns, wall_ns, slices, acquired, stamps) = self
            .active
            .with(&slot.id, |r| {
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
                // No clock read: `queued` started life as the submit path's
                // own `enqueued` instant and `admitted` keeps it, precisely so
                // this can be expressed on the shared origin for free.
                admitted_mono_ns: crate::mono_ns_at(s.admitted),
                submit_ns: s.submit_ns,
                queue_ns: s.queue_ns,
                guest_ns: wall_ns,
                // Overwritten by `Cell::respond` at the send. A zero that
                // escaped to a reader would mean the result was built and
                // never sent, which is not a path that answers anybody.
                replied_mono_ns: 0,
            }),
        }
    }

    fn respond(&self, slot: &Arc<Slot>, result: DispatchResult) {
        let tx = self
            .active
            .with_mut(&slot.id, |a| a.responder.take())
            .flatten();
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
        let req = self.active.remove(&slot.id);
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

    /// The books for a tombstone raised somewhere other than `settle`.
    ///
    /// So far there is exactly one such place: a guest that traps or is killed
    /// inside the pre-capture `wisp-gc-hint`, on the idle sweeper's thread and
    /// with no request in flight. `settle` does all of this inline for the
    /// dispatch path, and this exists rather than a call into `settle` because
    /// most of what `settle` does — a response, a breaker verdict, a
    /// disposition — has no meaning when there is no request to answer.
    ///
    /// The three increments are the ones §6.4's identity is built out of:
    /// `tombstones == hard_kills + traps`. Leaving them out would make a
    /// tombstone the cell definitely took invisible to
    /// [`Metrics::tombstone_invariant`] in the direction that *fails* the
    /// check, which is at least loud — but `traps` would also under-report,
    /// and the blast table's tombstone row is the one §10 reads.
    ///
    /// The overage is reported as zero. The hint runs on `CpuBudget::http`, a
    /// cap rather than a budget the tenant bought, so there is no meaningful
    /// figure to record past; the kill is counted, the CPU it burned is recorded
    /// where all CPU is recorded, and no overage is claimed.
    fn record_offpath_tombstone(
        &self,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        completion: Completion,
    ) {
        if completion == Completion::BudgetKill {
            entry.meters.record_budget_kill(KillPhase::Hard, 0);
            self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
            self.metrics.hard_kills.fetch_add(1, Ordering::Relaxed);
        } else {
            entry.meters.record_trap();
            self.metrics.traps.fetch_add(1, Ordering::Relaxed);
        }
        self.metrics.tombstones.fetch_add(1, Ordering::Relaxed);
        self.remember_tombstone(slot);
        // Out of `residents`, `index` and `durables`: the instance is gone and
        // the slot is only a refusal now. Without this it would stay in every
        // gauge and every sweep for the life of the cell, and the sweep would
        // keep selecting an instance it can never capture.
        self.forget(slot);
        tracing::warn!(
            tenant = %entry.tenant, instance = %slot.id, ?completion,
            "the guest ended its pre-capture gc-hint in a tombstone; the instance is \
             discarded and the slot keeps refusing (§6.4)"
        );
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
    use std::sync::Barrier;

    // -----------------------------------------------------------------------
    // fixtures
    // -----------------------------------------------------------------------

    /// A `DispatchJob` with nothing behind it, for exercising the queue.
    ///
    /// The queue never looks inside a job — it moves boxes between lanes — so
    /// the payload can be minimal and the test stays about the structure.
    fn job() -> Box<DispatchJob> {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        job_on(tx, 1)
    }

    /// The same, on a **caller-provided** channel and with a chosen id.
    ///
    /// This is `Cell::submit_with`'s shape: many jobs, one receiver, matched
    /// up by `dispatch_id`. A test that wants to know whether every admitted
    /// job was answered needs exactly that, because one channel per job
    /// cannot distinguish "answered" from "the sender was dropped".
    fn job_on(
        responder: std::sync::mpsc::SyncSender<DispatchResult>,
        dispatch_id: u64,
    ) -> Box<DispatchJob> {
        Box::new(DispatchJob {
            module: ModuleId::new("t", "v1"),
            target: Target::Handler { route: "/".into() },
            rec: wisp_abi::dispatch::DispatchRecord {
                dispatch_id,
                kind: crate::cell::cell_tests::http(dispatch_id).kind,
                trace: Default::default(),
                cancel_future: 0,
                cpu_budget_ms: 50,
                now_ms: 0,
            },
            responder,
            enqueued: Instant::now(),
            stamps: None,
        })
    }

    /// One real slot, for the wake lane.
    ///
    /// A wake is an `Arc<Slot>` and the queue only moves it, so one is enough
    /// for any number of wakes: the tests below clone the `Arc` rather than
    /// activate hundreds of instances, which would be measuring the fake.
    fn a_slot() -> Arc<Slot> {
        let pool = InstancePool::new(
            Arc::new(crate::fakes::FakeActivator::new(
                crate::fakes::FakeBehaviour::done(),
            )),
            HandlerLifecycle::Reuse,
            16,
        );
        let (slot, _) = pool
            .acquire(|id| {
                Arc::new(InstanceCtx::for_test(
                    id,
                    HandlerLifecycle::Reuse,
                    Limits::default(),
                ))
            })
            .expect("a slot");
        slot
    }

    fn wake_of(slot: &Arc<Slot>) -> WakeItem {
        WakeItem {
            slot: slot.clone(),
            queued: None,
        }
    }

    /// Join, or fail — never hang.
    ///
    /// Every wait in this module goes through here. A bare `join` turns a
    /// regression in a queue into a CI job that runs until somebody kills it,
    /// and the failure a lost wake-up produces is precisely a thread that
    /// never returns: unbounded, that is the *only* symptom, and it is
    /// indistinguishable from a slow machine until the timeout somebody else
    /// owns fires.
    pub(super) fn join_before<T>(h: std::thread::JoinHandle<T>, within: Duration, what: &str) -> T {
        let deadline = Instant::now() + within;
        while !h.is_finished() {
            assert!(
                Instant::now() < deadline,
                "{what}: still running after {within:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        h.join().expect("thread panicked")
    }

    // -----------------------------------------------------------------------
    // the submit stage, measured directly
    // -----------------------------------------------------------------------

    /// **`submit_ns` is the interval it names, in the unit it names.**
    ///
    /// The lower bound the stage never had, and the one that needs no threads,
    /// no contention and no wall-clock luck: backdate `enqueued` by a known 50
    /// ms and require the stamp to report it. Every assertion on `submit_ns`
    /// in `tests/stages.rs` is one-sided the other way — it appears only
    /// inside `submit + queue + guest <= observed` — so replacing the whole
    /// computation with a literal `0` passed that entire file.
    ///
    /// A magnitude and not just a non-zero, because the two failures worth
    /// separating fail differently: a stage that is never stamped reads 0, and
    /// a stage recorded in the wrong unit reads 50,000 or 50 rather than
    /// 50,000,000. Only a bound with a number in it catches the second.
    #[test]
    fn the_submit_stage_measures_the_interval_it_names() {
        const BACK: Duration = Duration::from_millis(50);
        let mut j = job();
        let admitted = Instant::now()
            .checked_sub(BACK)
            .expect("a monotonic clock past 50 ms");
        j.enqueued = admitted;
        j.stamps = Some(StageStamps {
            admitted,
            queued: admitted,
            submit_ns: 0,
            queue_ns: 0,
        });

        WorkQueue::stamp_queued(&mut j);

        let s = j.stamps.expect("the stamps survive the call");
        assert!(
            s.submit_ns >= BACK.as_nanos() as u64 * 9 / 10,
            "the job entered the submit path {BACK:?} ago and the stage reports {} ns",
            s.submit_ns
        );
        assert!(
            s.submit_ns < BACK.as_nanos() as u64 * 4,
            "the stage reports {} ns for a {BACK:?} interval, which is not a slow machine",
            s.submit_ns
        );
        // The other half of the one clock read: the queue stage opens where
        // the submit stage closed, with nothing between them.
        assert!(
            s.queued.duration_since(admitted).as_nanos() as u64 >= s.submit_ns,
            "the queue stage opens before the submit stage closed, so the two overlap"
        );
    }

    // -----------------------------------------------------------------------
    // structure and depth are two settings, and neither derives the other
    // -----------------------------------------------------------------------

    /// **The structure sets the lane count. It does not set the lane depth.**
    ///
    /// The depth used to be `(cap / lanes).max(1)`, which made two things
    /// false at once. A lane's depth moved with `exec_threads`, so the sweep
    /// that exists to vary the thread count varied the buffering with it and
    /// attributed the result to threads. And the `.max(1)` floor overshot in
    /// the other direction: a cap of 4 across 64 workers gave each lane one
    /// slot and the cell 64, against the 4 that was asked for.
    #[test]
    fn the_structure_sets_the_lane_count_and_not_the_lane_depth() {
        let q = WorkQueue::new(1024, 8, QueueStructure::Shared, QueuePlacement::PowerOfTwo);
        assert_eq!(q.lanes.len(), 1);
        assert_eq!(q.cap, 1024, "one lane holds what it was configured to hold");

        let q = WorkQueue::new(
            128,
            8,
            QueueStructure::PerWorker,
            QueuePlacement::PowerOfTwo,
        );
        assert_eq!(q.lanes.len(), 8);
        assert_eq!(
            q.cap, 128,
            "the configured depth, not the configured depth divided by 8"
        );

        // The floor that used to be reached by division is now not reachable
        // at all: a small depth is a small depth in every lane.
        let q = WorkQueue::new(4, 64, QueueStructure::PerWorker, QueuePlacement::PowerOfTwo);
        assert_eq!(q.lanes.len(), 64);
        assert_eq!(
            q.cap, 4,
            "64 workers must not turn a depth of 4 into a depth of 1"
        );
    }

    /// **Changing the thread count does not change the queue depth.**
    ///
    /// This is the property the exec-thread sweep needs and did not have. The
    /// whole-cell capacity does move, because it is the product and cannot
    /// not; the point is that it is now the *derived* number and is published
    /// as one, rather than the depth of a lane being derived silently.
    #[test]
    fn the_configured_queue_depth_does_not_move_with_the_thread_count() {
        let at = |threads: usize| CellConfig {
            exec_threads: threads,
            queue_structure: QueueStructure::PerWorker,
            queue_depth_per_worker: 64,
            admission_queue: 1024,
            ..CellConfig::for_test("depth")
        };
        for threads in [4usize, 8, 16, 64] {
            let c = at(threads);
            assert_eq!(
                c.depth_per_queue(),
                64,
                "at {threads} threads a lane holds {} rather than the 64 configured",
                c.depth_per_queue()
            );
            assert_eq!(c.queue_count(), threads);
            assert_eq!(c.admission_capacity(), 64 * threads);
        }
        // Under a single-queue structure the two numbers coincide, which is
        // why nothing that ran under `Shared` moves.
        let shared = CellConfig {
            queue_structure: QueueStructure::Shared,
            admission_queue: 1024,
            exec_threads: 64,
            ..CellConfig::for_test("depth")
        };
        assert_eq!(shared.queue_count(), 1);
        assert_eq!(shared.depth_per_queue(), 1024);
        assert_eq!(shared.admission_capacity(), 1024);
    }

    // -----------------------------------------------------------------------
    // placement, which is now its own setting
    // -----------------------------------------------------------------------

    /// Power of two picks the less loaded of the two it sampled.
    ///
    /// Checked statistically rather than by forcing the sample, because the
    /// property that matters is the distribution: with one lane loaded and the
    /// rest idle, a balancer that works almost never chooses the loaded one,
    /// and one that ignores load chooses it 1-in-N of the time.
    #[test]
    fn power_of_two_avoids_the_loaded_lane() {
        let q = WorkQueue::new(
            4096,
            8,
            QueueStructure::PerWorker,
            QueuePlacement::PowerOfTwo,
        );
        q.lanes[3].load.store(1_000, Ordering::Relaxed);

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
            "the loaded lane was chosen {hit_deep} times in {N} — power of two should sample \
             it twice before choosing it"
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

    /// **Round robin hands out every lane in turn, and ignores load doing it.**
    ///
    /// Both halves are the policy: perfectly even counts is what it is for,
    /// and blindness to what a lane is carrying is what it is a control for.
    /// A round robin that quietly consulted the load would not be one.
    #[test]
    fn round_robin_hands_out_every_lane_in_turn_and_ignores_load() {
        let q = WorkQueue::new(64, 8, QueueStructure::PerWorker, QueuePlacement::RoundRobin);
        q.lanes[3].load.store(1_000, Ordering::Relaxed);
        let first: Vec<usize> = (0..16).map(|_| q.pick()).collect();
        let start = first[0];
        for (i, got) in first.iter().enumerate() {
            assert_eq!(
                *got,
                (start + i) % 8,
                "round robin skipped a lane at step {i}: {first:?}"
            );
        }
        assert!(
            first.contains(&3),
            "round robin must not steer around the loaded lane"
        );
    }

    /// Random placement reaches every lane and prefers none of them.
    ///
    /// It is the other control: if power of two does not beat this, the load
    /// counter it consults is not carrying information.
    #[test]
    fn random_placement_reaches_every_lane() {
        let q = WorkQueue::new(64, 8, QueueStructure::PerWorker, QueuePlacement::Random);
        q.lanes[3].load.store(1_000, Ordering::Relaxed);
        let mut counts = [0usize; 8];
        const N: usize = 20_000;
        for _ in 0..N {
            counts[q.pick()] += 1;
        }
        for (lane, n) in counts.iter().enumerate() {
            assert!(
                *n > N / 16 && *n < N / 4,
                "lane {lane} took {n} of {N}, which is not a uniform draw over 8 lanes: \
                 {counts:?}"
            );
        }
    }

    /// **Placement is inert, not wrong, when there is only one queue.**
    ///
    /// Every policy has to pick lane zero under `Shared`, because there is
    /// nothing else to pick. A policy that indexed past the end here would
    /// panic on the first dispatch of a run that set both settings without
    /// realising only one of them applied.
    #[test]
    fn placement_is_inert_when_the_structure_builds_one_queue() {
        for placement in [
            QueuePlacement::PowerOfTwo,
            QueuePlacement::RoundRobin,
            QueuePlacement::Random,
        ] {
            let q = WorkQueue::new(64, 16, QueueStructure::Shared, placement);
            assert_eq!(q.lanes.len(), 1);
            for _ in 0..64 {
                assert_eq!(
                    q.pick(),
                    0,
                    "{} picked a lane that does not exist",
                    placement.label()
                );
            }
            assert!(
                !QueueStructure::Shared.placement_applies(),
                "a report must not claim {} was exercised here",
                placement.label()
            );
        }
    }

    // -----------------------------------------------------------------------
    // what a lane's load counter can see
    // -----------------------------------------------------------------------

    /// **A lane keeps counting the job its worker is still running.**
    ///
    /// The counter used to be decremented by the pop, so a worker twenty
    /// seconds into a thirty-second dispatch sat on a lane that read zero —
    /// the most attractive lane in the cell to a placer whose whole job is to
    /// avoid the busy one.
    #[test]
    fn a_lane_keeps_counting_the_job_its_worker_is_still_running() {
        let q = WorkQueue::new(8, 1, QueueStructure::PerWorker, QueuePlacement::PowerOfTwo);
        assert!(q.push_dispatch(job()).is_ok());
        let load = || q.lanes[0].load.load(Ordering::Relaxed);
        assert_eq!(load(), 1, "a queued dispatch is load");

        let taken = q.pop(0).expect("the job");
        assert_eq!(
            load(),
            1,
            "the worker is inside the guest with this job; the lane still owes it and a \
             placer that reads zero here will send it another"
        );
        drop(taken);
        q.job_done(0);
        assert_eq!(
            load(),
            0,
            "and it is released when the job is finished, not when it is taken"
        );
    }

    /// **Queued wakes count toward the lane that is holding them.**
    ///
    /// `push_wake` never touched the counter, so a lane holding four hundred
    /// wakes — every one of them a request already in flight, and the priority
    /// lane at that — read exactly zero.
    #[test]
    fn queued_wakes_count_toward_the_lane_that_is_holding_them() {
        let q = WorkQueue::new(8, 4, QueueStructure::PerWorker, QueuePlacement::RoundRobin);
        let slot = a_slot();
        for _ in 0..400 {
            q.push_wake(wake_of(&slot));
        }
        let loads: Vec<usize> = q
            .lanes
            .iter()
            .map(|l| l.load.load(Ordering::Relaxed))
            .collect();
        assert_eq!(
            loads.iter().sum::<usize>(),
            400,
            "400 wakes are queued and the lanes report {loads:?}"
        );
        for (lane, n) in loads.iter().enumerate() {
            assert_eq!(
                *n, 100,
                "round robin put {n} of the 400 wakes in lane {lane}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // the two lanes, in every structure
    // -----------------------------------------------------------------------

    /// **A wake comes out before a dispatch, whatever the structure.**
    ///
    /// §6.4's admission story: overflow sheds *new* work so work already in
    /// flight keeps making progress. It is a property of what a worker pops,
    /// not of where a producer pushed, which is why splitting placement out
    /// cannot weaken it — but "cannot" is worth one assertion per structure.
    ///
    /// One worker, so all three structures hold exactly one queue and the
    /// order is observable rather than a function of which lane won a draw.
    #[test]
    fn every_structure_pops_wakes_before_dispatches() {
        let slot = a_slot();
        for structure in [
            QueueStructure::Shared,
            QueueStructure::PerWorker,
            QueueStructure::LockFree,
        ] {
            let q = WorkQueue::new(8, 1, structure, QueuePlacement::PowerOfTwo);
            assert!(q.push_dispatch(job()).is_ok(), "{}", structure.label());
            q.push_wake(wake_of(&slot));
            assert!(
                matches!(q.pop(0), Some(Job::Wake(_))),
                "{}: the wake queued second must come out first",
                structure.label()
            );
            assert!(
                matches!(q.pop(0), Some(Job::Dispatch(_))),
                "{}: and the dispatch after it",
                structure.label()
            );
            q.close();
        }
    }

    /// **Under `Shared` every worker index reads the one lane.**
    ///
    /// The test that stood here asserted `worker % 1 == 0` — arithmetic it
    /// performed on itself — and never called `pop`, so replacing the routing
    /// with a constant passed it. This pushes and pops: sixteen jobs into one
    /// lane, drained by sixteen different worker indices.
    ///
    /// On a thread and bounded, because the failure it is looking for is a
    /// worker routed to a lane of its own, and a worker on an empty lane
    /// *blocks*. Unbounded, the regression would hang CI rather than fail it.
    #[test]
    fn under_shared_every_worker_pops_from_the_one_lane() {
        let q = Arc::new(WorkQueue::new(
            64,
            16,
            QueueStructure::Shared,
            QueuePlacement::PowerOfTwo,
        ));
        assert_eq!(
            q.lanes.len(),
            1,
            "`Shared` is one lane however many workers there are"
        );
        for i in 0..16 {
            assert!(q.push_dispatch(job()).is_ok(), "job {i} inside the cap");
        }
        assert_eq!(q.depth(), 16);

        let q2 = q.clone();
        let h = std::thread::spawn(move || {
            (0..16usize)
                .filter(|w| matches!(q2.pop(*w), Some(Job::Dispatch(_))))
                .count()
        });
        assert_eq!(
            join_before(
                h,
                Duration::from_secs(10),
                "sixteen workers draining one lane"
            ),
            16,
            "every worker index must be able to take work from the shared lane"
        );
        assert_eq!(q.depth(), 0);
    }

    // -----------------------------------------------------------------------
    // the lock-free structure
    // -----------------------------------------------------------------------

    /// The lock-free ring sheds when full, which is the property §6.4 needs
    /// and the reason a *bounded* queue was chosen for dispatches.
    #[test]
    fn the_lock_free_ring_sheds_when_full() {
        let q = WorkQueue::new(4, 8, QueueStructure::LockFree, QueuePlacement::PowerOfTwo);
        assert!(
            q.lanes.is_empty(),
            "no mutex lanes are built for this structure"
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
        q.close();
    }

    /// **A worker confirmed parked is woken by the next push.**
    ///
    /// This is the race a lock-free queue introduces and a mutex does not: the
    /// producer must not slip its push and its notify between the consumer's
    /// last look at the queue and its going to sleep. If it can, a worker
    /// sleeps while work waits, and under load that shows up as a latency tail
    /// nobody can explain.
    ///
    /// The version that stood here spawned the consumer, called `yield_now`,
    /// and pushed. `yield_now` is a hint and not a barrier, so the consumer
    /// almost always took the fast path and the parking code was never
    /// reached: deleting the whole guard left it passing twenty runs out of
    /// twenty. This one does not push until `parked` says a consumer has
    /// announced itself, and `parked` is only ever incremented on the slow
    /// path — so the fast path cannot satisfy this test, and a producer that
    /// does not notify a parked consumer fails it every time rather than one
    /// round in two hundred.
    #[test]
    fn a_worker_confirmed_parked_is_woken_by_the_next_push() {
        for round in 0..50 {
            let q = Arc::new(WorkQueue::new(
                64,
                4,
                QueueStructure::LockFree,
                QueuePlacement::PowerOfTwo,
            ));
            let q2 = q.clone();
            let h = std::thread::spawn(move || q2.pop(0).is_some());

            let f = q.free.as_ref().expect("the lock-free lanes");
            let deadline = Instant::now() + Duration::from_secs(10);
            while f.parked.load(Ordering::SeqCst) == 0 {
                assert!(
                    Instant::now() < deadline,
                    "round {round}: no consumer ever announced a park, so this test is not \
                     exercising the parking path at all"
                );
                std::hint::spin_loop();
            }
            assert!(q.push_dispatch(job()).is_ok());
            assert!(
                join_before(
                    h,
                    Duration::from_secs(10),
                    &format!("round {round}: parked worker")
                ),
                "round {round}: the worker returned without the work that was pushed to it"
            );
        }
    }

    /// Closing wakes every parked worker, or shutdown hangs.
    #[test]
    fn closing_the_lock_free_queue_releases_parked_workers() {
        let q = Arc::new(WorkQueue::new(
            64,
            4,
            QueueStructure::LockFree,
            QueuePlacement::PowerOfTwo,
        ));
        let hs: Vec<_> = (0..4)
            .map(|w| {
                let q = q.clone();
                std::thread::spawn(move || q.pop(w))
            })
            .collect();
        // Every worker has to be genuinely parked before the close, or the
        // test proves only that a closed queue returns `None` to a caller that
        // never slept.
        let f = q.free.as_ref().expect("the lock-free lanes");
        let deadline = Instant::now() + Duration::from_secs(10);
        while f.parked.load(Ordering::SeqCst) < 4 {
            assert!(Instant::now() < deadline, "only some workers ever parked");
            std::hint::spin_loop();
        }
        q.close();
        for (w, h) in hs.into_iter().enumerate() {
            assert!(
                join_before(
                    h,
                    Duration::from_secs(10),
                    &format!("worker {w} after close")
                )
                .is_none(),
                "a closed queue returns None"
            );
        }
    }

    /// **A dispatch is either shed or served — a close never strands one.**
    ///
    /// `push_dispatch` on the lock-free path checked `closed` and then pushed,
    /// which is two steps: a producer can read `closed` as false, lose the
    /// cpu, and push into a ring every worker has already drained and left.
    /// That job is never run and never refused, and its caller — who was told
    /// `true` and is holding a receiver — waits for a reply nobody will send.
    /// **Strengthening the atomic ordering does not fix it**; the check and
    /// the push are still two operations on two locations.
    ///
    /// So this races a burst of pushes against a close and demands that the
    /// number of jobs answered on the shared channel equals the number
    /// admitted. Every job that was admitted must come back; every job that
    /// was not must have been refused at the push. The assertion is an
    /// equality rather than an inequality, so it also catches a job answered
    /// twice.
    ///
    /// **The assertion is exact; reaching the window is not.** The race is a
    /// few instructions wide and there is no hook to suspend a producer inside
    /// it, so this is a sampler and says so rather than claiming otherwise.
    /// Two things make it a reliable one:
    ///
    /// * **Productive rounds are counted, not rounds.** A round where the
    ///   close won outright admits nothing and satisfies the equality without
    ///   racing anything. Counting those was how whole runs used to pass with
    ///   the guard removed.
    /// * **Eight producers rather than two.** Each close races whichever
    ///   producers are in the window at that instant, so the odds scale with
    ///   the thread count and not with how many jobs each pushes. Moving the
    ///   same burst from 2x64 to 8x16 was both stronger and faster.
    ///
    /// Measured, because a claim about a sampler is worth exactly its
    /// measurement: with the guard removed this was red in 25 runs of 25, and
    /// green in 15 of 15 with it. Its predecessor — 400 rounds of 2x64, no
    /// productive-round accounting — was red in about 9 runs of 10, and a
    /// whole run passing with the defect present is what the accounting fixes.
    /// At 1,000 productive rounds it falls back to 18 of 20, so the count is
    /// load-bearing and not padding.
    #[test]
    fn a_lock_free_dispatch_is_either_shed_or_answered_and_never_stranded() {
        // Counted in **productive** rounds — rounds in which at least one push
        // was admitted — because a round where the close won outright admits
        // nothing and satisfies the equality without racing anything. Counting
        // those was how whole runs used to pass with the guard removed.
        const PRODUCTIVE_ROUNDS: usize = 2_000;
        const MAX_ROUNDS: usize = 40_000;
        // **Eight producers, not two.** Each close races every producer that
        // is in the window at that instant, so the per-round odds scale with
        // the thread count and not with how many jobs each one pushes. Moving
        // the same total burst from 2x64 to 8x16 took the guard-removed
        // failure rate from 7 runs in 12 to 10 in 10, and made the test
        // faster.
        const PRODUCERS: usize = 8;
        const PER_PRODUCER: usize = 16;
        let mut productive = 0usize;
        let mut round = 0usize;
        while productive < PRODUCTIVE_ROUNDS {
            round += 1;
            assert!(
                round <= MAX_ROUNDS,
                "only {productive} of {MAX_ROUNDS} rounds admitted anything at all; the close \
                 is winning outright every time and this test is not racing"
            );
            let q = Arc::new(WorkQueue::new(
                PRODUCERS * PER_PRODUCER,
                4,
                QueueStructure::LockFree,
                QueuePlacement::PowerOfTwo,
            ));
            let (tx, rx) = std::sync::mpsc::sync_channel(PRODUCERS * PER_PRODUCER * 2);
            // A `Barrier`, and **the gate is not the load-bearing part** —
            // measured, because a comment written to stop the next person
            // simplifying this had better say what actually matters.
            // Substituting the tightest release available (an arrival counter
            // so all eight are confirmed hot-spinning, then one `AtomicBool`)
            // catches the guard-removed mutation 12 times in 12, which is the
            // barrier's own rate. Simplify the gate if you like.
            //
            // What *is* load-bearing, in order: the productive-round
            // accounting above, the producer count, and closing here rather
            // than later. Deferring the close until the ring is non-empty —
            // so the drain lands mid-burst — drops it to 6 in 12, because the
            // drain is a `while let Some` loop and mops up the stragglers it
            // is supposed to be racing.
            //
            // An earlier version of this comment claimed a hot-spinning gate
            // was markedly *worse*, at 0 of 10. That was measured on the
            // 2-producer, 400-round version with no productive-round
            // accounting, where the observed failure was "no round admitted
            // anything" — the test unable to run, which is not the same event
            // as the mutation going uncaught, and is exactly what the
            // accounting now retries instead of counting. The number was real
            // and the configuration it described no longer exists.
            let gate = Arc::new(Barrier::new(PRODUCERS + 1));

            let producers: Vec<_> = (0..PRODUCERS)
                .map(|p| {
                    let q = q.clone();
                    let tx = tx.clone();
                    let gate = gate.clone();
                    std::thread::spawn(move || {
                        // Built *before* the start signal. A `DispatchJob` is
                        // a boxed record of several `String`s, and allocating
                        // one takes far longer than the window this is trying
                        // to land in — build them inside the loop and every
                        // producer reads `closed` hundreds of nanoseconds
                        // after the close, so nothing is ever admitted and the
                        // race is never run.
                        let jobs: Vec<_> = (0..PER_PRODUCER)
                            .map(|i| job_on(tx.clone(), (p * PER_PRODUCER + i) as u64))
                            .collect();
                        drop(tx);
                        gate.wait();
                        jobs.into_iter()
                            .fold(0usize, |n, j| n + usize::from(q.push_dispatch(j).is_ok()))
                    })
                })
                .collect();
            drop(tx);
            gate.wait();
            q.close();

            let admitted: usize = producers
                .into_iter()
                .map(|h| {
                    join_before(
                        h,
                        Duration::from_secs(10),
                        &format!("round {round}: producer"),
                    )
                })
                .sum();
            if admitted > 0 {
                productive += 1;
            }
            // Both sides are finished: every producer has returned and `close`
            // is synchronous, so everything that will ever be sent has been.
            // `try_iter` therefore counts exactly, and does not deadlock on a
            // sender still sitting in a stranded job.
            let answered = rx.try_iter().count();
            assert_eq!(
                answered, admitted,
                "round {round}: {admitted} dispatches were admitted and {answered} were \
                 answered — the difference is stranded in the ring, and its caller is \
                 waiting on a channel nothing will ever be sent on"
            );
        }
    }
}

/// Cell-level tests: what survives a host panic, and what the cell says about
/// the queue it is running.
///
/// They share one harness, which is why they share a module.
#[cfg(test)]
mod cell_tests {
    use super::*;
    use crate::fakes::*;
    use wisp_abi::config::{AppManifest, Bindings, HandlerConfig};

    // -----------------------------------------------------------------------
    // a guest that panics the host rather than trapping
    // -----------------------------------------------------------------------

    /// An "instance" that panics inside `dispatch`, standing in for a host
    /// assertion firing under a guest call.
    ///
    /// The one that has actually fired is §6.4's tombstone assertion, which is
    /// a `panic!` on purpose. Any other `assert!` or slice index in this crate
    /// reaches an execution thread the same way.
    struct PanicsOnDispatch;

    impl wisp_engine::InstanceOps for PanicsOnDispatch {
        fn dispatch(
            &mut self,
            _rec: &DispatchRecord,
            _b: CpuBudget,
        ) -> wisp_engine::DispatchOutcome {
            panic!("a host assertion fired underneath a dispatch");
        }
        fn wake(
            &mut self,
            _f: u64,
            _r: &wisp_abi::result::ResultRecord,
            _b: CpuBudget,
        ) -> wisp_engine::DispatchOutcome {
            panic!("a host assertion fired underneath a wake");
        }
        fn gc_hint(&mut self, _p: u8) -> wisp_engine::DispatchOutcome {
            unreachable!("this fixture never reaches the hibernation path")
        }
        fn reset(&mut self) -> anyhow::Result<()> {
            Ok(())
        }
        fn is_parked(&self) -> bool {
            false
        }
        fn is_capture_eligible(&self) -> bool {
            false
        }
        fn memory(&mut self) -> &mut [u8] {
            &mut []
        }
        fn memory_len(&self) -> usize {
            0
        }
        fn capture_mutable_state(&mut self) -> anyhow::Result<wisp_engine::InstanceImage> {
            Ok(Default::default())
        }
        fn restore_mutable_state(&mut self, _i: &wisp_engine::InstanceImage) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct PanicActivator {
        module: ModuleId,
        template: Arc<Vec<u8>>,
    }

    impl crate::instances::Activator for PanicActivator {
        fn module(&self) -> &ModuleId {
            &self.module
        }
        fn module_hash(&self) -> wisp_engine::ModuleHash {
            wisp_engine::ModuleHash::of(&self.template)
        }
        fn abi_rev(&self) -> u32 {
            wisp_abi::ABI_REV
        }
        fn template_image(&self) -> Arc<Vec<u8>> {
            self.template.clone()
        }
        fn activate(
            &self,
            _b: Box<dyn wisp_engine::HostBindings>,
        ) -> anyhow::Result<Box<dyn wisp_engine::InstanceOps + Send>> {
            Ok(Box::new(PanicsOnDispatch))
        }
    }

    // -----------------------------------------------------------------------
    // harness
    // -----------------------------------------------------------------------

    struct NoResolver;
    impl crate::Resolver for NoResolver {
        fn resolve(&self, _h: &str, _p: u16) -> Result<Vec<std::net::SocketAddr>, String> {
            Ok(vec![])
        }
    }

    fn services() -> Arc<Services> {
        Arc::new(Services {
            egress: Arc::new(crate::egress::HostEgressPolicy::new(
                Box::new(NoResolver),
                64,
                1 << 20,
            )),
            fetcher: Arc::new(FakeFetcher::default()),
        })
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

    fn http(id: u64) -> DispatchRecord {
        DispatchRecord {
            dispatch_id: id,
            kind: DispatchKind::Http(wisp_abi::HttpRequest {
                method: "GET".into(),
                url: "http://app.example/go".into(),
                path: "/go".into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: 1_000,
            now_ms: 0,
        }
    }

    fn app(cell: &Arc<Cell>, name: &str, behaviour: FakeBehaviour) -> ModuleId {
        let act = Arc::new(FakeActivator::named(ModuleId::new(name, "v1"), behaviour));
        let module = act.module().clone();
        cell.register_module("acme", act, manifest(name)).unwrap();
        module
    }

    fn wait_for(mut p: impl FnMut() -> bool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if p() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }

    // -----------------------------------------------------------------------
    // what the cell reports about the queue it built
    // -----------------------------------------------------------------------

    /// **The contention row says which structure produced it.**
    ///
    /// `WorkQueue::strategy` and the field behind it were never read — two
    /// dead-code warnings — while `Cell::lock_stats`'s own doc promised "the
    /// report says which strategy produced it". It matters because one row
    /// means three different things: 365 s of blocked time under `shared` is
    /// one mutex every thread queued on; the same figure under `per-worker` is
    /// that time spread over `exec_threads` separate locks; and a zero under
    /// `lock-free` is "there is no lock to block on", which is not the same
    /// statement as "nothing blocked". Two runs' tables are not comparable
    /// without it.
    #[test]
    fn the_contention_row_names_the_structure_that_produced_it() {
        for (structure, row) in [
            (QueueStructure::Shared, "WorkQueue::inner[shared]"),
            (QueueStructure::PerWorker, "WorkQueue::inner[per-worker]"),
            // No row expected for this one — see the branch below.
            (QueueStructure::LockFree, ""),
        ] {
            let cell = Cell::new(
                CellConfig {
                    exec_threads: 2,
                    queue_structure: structure,
                    queue_placement: QueuePlacement::RoundRobin,
                    ..CellConfig::for_test("rows")
                },
                services(),
            );
            let module = app(&cell, "quick", FakeBehaviour::done());
            let out = cell
                .dispatch_blocking(
                    &module,
                    Target::Handler {
                        route: "/go".into(),
                    },
                    http(1),
                    Duration::from_secs(10),
                )
                .expect("a dispatch under every structure");
            assert!((200..300).contains(&out.status), "{structure:?}: {out:?}");

            assert_eq!(
                cell.queue_structure(),
                structure,
                "the cell must report what it built"
            );
            assert_eq!(cell.queue_placement(), QueuePlacement::RoundRobin);
            let stats = cell.lock_stats();
            let names: Vec<&str> = stats.iter().map(|(n, _)| *n).collect();
            if structure == QueueStructure::LockFree {
                // Rule 2: the lock does not exist, so there is no row. A zero
                // row here would read as "measured, nothing blocked", which is
                // a different and false statement.
                assert!(
                    !names.iter().any(|n| n.starts_with("WorkQueue::inner")),
                    "lock-free has no work-queue lock, so it must contribute no row: {names:?}"
                );
            } else {
                assert!(
                    names.contains(&row),
                    "{structure:?}: no row named `{row}` in {names:?}"
                );
            }
            // Rule 1 is a rule, not a special case for one row: every suffix
            // that appears has to be a `[...]` at the end of the name.
            for n in &names {
                if let Some(open) = n.find('[') {
                    assert!(
                        n.ends_with(']') && open > 0,
                        "row `{n}` has a malformed configuration suffix"
                    );
                }
            }
            cell.shutdown();
        }
    }

    /// **`Cell::active` carries its structure in the row name too.**
    ///
    /// Rule 1 is a rule and not a special case for the work queue. This row is
    /// one mutex under `Plain` and 256 shards summed under `Sharded` — the
    /// same figure meaning two different things — and a run comparing the two
    /// implementations, which is the entire reason the map is selectable, is
    /// exactly the run that must not read one for the other.
    #[test]
    fn the_active_map_row_names_the_structure_that_produced_it() {
        for (kind, row) in [
            (
                crate::lockstats::ActiveMapKind::Plain,
                "Cell::active[plain]",
            ),
            (
                crate::lockstats::ActiveMapKind::Sharded,
                "Cell::active[sharded]",
            ),
        ] {
            let cell = Cell::new(
                CellConfig {
                    active_map: kind,
                    ..CellConfig::for_test("active-row")
                },
                services(),
            );
            let m = app(&cell, "quick", FakeBehaviour::done());
            let out = cell
                .dispatch_blocking(
                    &m,
                    Target::Handler {
                        route: "/go".into(),
                    },
                    http(1),
                    Duration::from_secs(10),
                )
                .expect("a dispatch under either map");
            assert!((200..300).contains(&out.status), "{kind:?}: {out:?}");
            assert_eq!(
                cell.active_map(),
                kind,
                "the cell must report the map it built"
            );

            let names: Vec<&str> = cell.lock_stats().iter().map(|(n, _)| *n).collect();
            assert!(
                names.contains(&row),
                "{kind:?}: no row named `{row}` in {names:?}"
            );
            cell.shutdown();
        }
    }

    /// **The published admission capacity is the one the cell will actually
    /// hold.**
    ///
    /// Under `PerWorker` it is not `admission_queue`, and a report that
    /// recorded that field would be recording a number the cell does not use.
    #[test]
    fn the_cell_publishes_the_queue_shape_it_is_running() {
        let cell = Cell::new(
            CellConfig {
                exec_threads: 4,
                queue_structure: QueueStructure::PerWorker,
                queue_depth_per_worker: 32,
                admission_queue: 1024,
                ..CellConfig::for_test("shape")
            },
            services(),
        );
        assert_eq!(cell.queue_shape(), (32, 4));
        assert_eq!(
            cell.admission_capacity(),
            128,
            "4 lanes of 32, not the 1024 `admission_queue` still holds"
        );
        cell.shutdown();
    }

    // -----------------------------------------------------------------------
    // the decomposition, under every queue configuration
    // -----------------------------------------------------------------------

    /// Every structure x placement combination the config can express.
    ///
    /// Nine, of which five are distinct behaviours — placement is inert under
    /// the two single-queue structures — and all nine are run anyway, because
    /// what is under test includes the claim that setting a placement under
    /// `Shared` does nothing rather than something quiet.
    fn every_queue_configuration() -> Vec<(QueueStructure, QueuePlacement)> {
        let mut v = Vec::new();
        for structure in [
            QueueStructure::Shared,
            QueueStructure::PerWorker,
            QueueStructure::LockFree,
        ] {
            for placement in [
                QueuePlacement::PowerOfTwo,
                QueuePlacement::RoundRobin,
                QueuePlacement::Random,
            ] {
                v.push((structure, placement));
            }
        }
        v
    }

    /// **The stage decomposition holds under every queue configuration, and
    /// every stage is a reading rather than an initialiser.**
    ///
    /// Before this, stage timing had only ever run under the default `Shared`:
    /// every test in `tests/stages.rs` takes the default, so the lock-free
    /// branch of `stamp_queued` — which stamps at a different point in the
    /// sequence from the mutex branch — had never once executed in a test.
    /// That is how a `submit_ns` meaning two different intervals survived.
    ///
    /// What is asserted here is the whole shape at once, per configuration:
    ///
    /// * the bracket runs forwards and contains the caller's own view of the
    ///   call, so the two stamps are on the clock the caller reads;
    /// * the three durations fit inside the bracket — `is_coherent`, which is
    ///   the no-overlap and no-unit-error check;
    /// * each of the three is **strictly positive**. This is the lower bound
    ///   `submit_ns` never had: it spans a mutex acquisition in the hang
    ///   breaker and a heap allocation, so zero nanoseconds is not a
    ///   measurement of it, it is the absence of one. `queue_ns` had such a
    ///   guard already and `submit_ns` did not, which is exactly why replacing
    ///   the latter with a literal `0` used to pass the entire suite.
    #[test]
    fn every_queue_configuration_produces_a_coherent_decomposition() {
        for (structure, placement) in every_queue_configuration() {
            let what = format!("{}/{}", structure.label(), placement.label());
            let cell = Cell::new(
                CellConfig {
                    exec_threads: 2,
                    stage_sample_every: 1,
                    queue_structure: structure,
                    queue_placement: placement,
                    ..CellConfig::for_test("stage-combo")
                },
                services(),
            );
            let m = app(
                &cell,
                "quick",
                FakeBehaviour::program([Step::Log {
                    bytes: 512,
                    times: 2_000,
                }]),
            );

            for id in 1..=3u64 {
                let before = crate::mono_ns();
                let out = cell
                    .dispatch_blocking(
                        &m,
                        Target::Handler {
                            route: "/go".into(),
                        },
                        http(id),
                        Duration::from_secs(20),
                    )
                    .unwrap_or_else(|e| panic!("{what}: dispatch {id} lost: {e:?}"));
                let after = crate::mono_ns();
                assert!((200..300).contains(&out.status), "{what}: {out:?}");
                let s = out.stages.unwrap_or_else(|| {
                    panic!("{what}: every request is sampled at 1 and this one carried nothing")
                });

                assert!(
                    s.admitted_mono_ns >= before && s.replied_mono_ns <= after,
                    "{what}: the cell bracketed the request [{}, {}] outside the caller's own \
                     [{before}, {after}] — the two ends are not reading the same clock",
                    s.admitted_mono_ns,
                    s.replied_mono_ns
                );
                assert!(
                    s.is_coherent(),
                    "{what}: submit {} + queue {} + guest {} exceeds the {} ns the cell held \
                     the request for — the stages overlap, or one is in the wrong unit",
                    s.submit_ns,
                    s.queue_ns,
                    s.guest_ns,
                    s.service_ns()
                );
                assert!(
                    s.submit_ns > 0,
                    "{what}: submit_ns is zero. It spans the hang breaker's mutex and a heap \
                     allocation; zero nanoseconds is not a reading of that, it is a stage that \
                     was never stamped"
                );
                assert!(s.queue_ns > 0, "{what}: queue_ns is zero");
                assert!(s.guest_ns > 0, "{what}: guest_ns is zero");
                // The residual is the point of publishing both stamps: it is
                // real work, and it must come out as a number rather than as
                // an acknowledgement in a doc comment.
                assert!(
                    s.residual_ns() < s.service_ns(),
                    "{what}: the residual is the whole request, so nothing was attributed"
                );
            }
            cell.shutdown();
        }
    }

    /// **`submit_ns` names the same interval under every structure.**
    ///
    /// The mutex branch used to stamp *after* acquiring the lane lock and the
    /// lock-free branch *before* its compare-and-swap, so `Shared` charged its
    /// contended acquisition to `submit` and `LockFree` charged its contended
    /// CAS to nothing at all. A report column comparing `submit` across
    /// structures compared two different quantities, and flattered the
    /// structure whose contention it was silently dropping.
    ///
    /// Equivalence of a *definition* is not something an assertion on two
    /// numbers can establish — the numbers legitimately differ, that is what
    /// measuring is for. What is checkable, and what this checks, is the
    /// consequence the mismatch had: no structure may report a submit stage
    /// that is structurally absent while another reports a real one. So the
    /// stage is required to be present, positive, and inside the bracket in
    /// all three, and it is required to be *small* relative to the whole
    /// request in all three — a submit stage that had swallowed a lock
    /// acquisition on one structure and not another shows up as exactly that
    /// asymmetry.
    #[test]
    fn submit_names_the_same_interval_under_every_structure() {
        for structure in [
            QueueStructure::Shared,
            QueueStructure::PerWorker,
            QueueStructure::LockFree,
        ] {
            let cell = Cell::new(
                CellConfig {
                    exec_threads: 2,
                    stage_sample_every: 1,
                    queue_structure: structure,
                    ..CellConfig::for_test("stage-submit")
                },
                services(),
            );
            let m = app(
                &cell,
                "quick",
                FakeBehaviour::program([Step::Log {
                    bytes: 512,
                    times: 4_000,
                }]),
            );
            let out = cell
                .dispatch_blocking(
                    &m,
                    Target::Handler {
                        route: "/go".into(),
                    },
                    http(1),
                    Duration::from_secs(20),
                )
                .expect("dispatch");
            let s = out.stages.expect("sampled");
            assert!(
                s.submit_ns > 0,
                "{}: no submit stage at all",
                structure.label()
            );
            assert!(
                s.submit_ns < s.service_ns(),
                "{}: submit_ns is {} of the {} ns the cell held the request — the stage stops \
                 before the enqueue in every structure, so it cannot be most of a request that \
                 ran a guest",
                structure.label(),
                s.submit_ns,
                s.service_ns()
            );
            cell.shutdown();
        }
    }

    /// **The enqueue is charged to `queue_ns`, and never to `submit_ns`.**
    ///
    /// This is the boundary decision made observable. `submit_ns` stops once
    /// placement is decided and before the enqueue begins, because that is the
    /// only point all three structures share — the enqueue moves the job out
    /// of the producer's hands, so no structure can be stamped after it. The
    /// consequence is that acquiring the intake lane's lock is queueing, not
    /// submitting.
    ///
    /// So the test takes that lock and holds it. A submit stage that still
    /// ended inside the lock — which is where it used to end, and is why
    /// `Shared` and `LockFree` were reporting different quantities under one
    /// column heading — puts the whole 300 ms into `submit_ns` and fails here.
    #[test]
    fn the_enqueue_is_charged_to_the_queue_stage_and_not_to_submit() {
        const HELD: Duration = Duration::from_millis(300);
        let cell = Cell::new(
            CellConfig {
                exec_threads: 1,
                max_hot: 1,
                stage_sample_every: 1,
                queue_structure: QueueStructure::Shared,
                ..CellConfig::for_test("stage-enqueue")
            },
            services(),
        );
        let m = app(&cell, "quick", FakeBehaviour::done());
        // Let the one worker reach `cv.wait`, which releases the lane lock, so
        // the hold below contends with the producer and not with the consumer.
        assert!(wait_for(|| cell.queue_depth() == 0, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));

        let held = cell.queue.lanes[0].inner.lock();
        let c2 = cell.clone();
        let m2 = m.clone();
        let submitting = std::thread::spawn(move || {
            c2.dispatch_blocking(
                &m2,
                Target::Handler {
                    route: "/go".into(),
                },
                http(1),
                Duration::from_secs(30),
            )
        });
        std::thread::sleep(HELD);
        drop(held);

        let out = super::queue_tests::join_before(
            submitting,
            Duration::from_secs(30),
            "held-lane dispatch",
        )
        .expect("dispatch");
        let s = out.stages.expect("sampled");
        assert!(
            s.queue_ns >= (HELD.as_nanos() as u64) * 2 / 3,
            "the producer waited {HELD:?} for the intake lane and the request reports {} ns of \
             queueing; waiting to get into a lane is waiting for a worker",
            s.queue_ns
        );
        assert!(
            s.submit_ns < HELD.as_nanos() as u64 / 3,
            "submit_ns is {} ns against a {HELD:?} lane hold — the stage is ending inside the \
             enqueue again, which is the asymmetry that made `submit` mean one thing under a \
             mutex and another under a ring",
            s.submit_ns
        );
        assert!(s.is_coherent(), "{s:?}");
        cell.shutdown();
    }

    /// **`submit_ns` responds to work done inside the submit stage.**
    ///
    /// The lower bound `submit_ns` never had. Every assertion on it in
    /// `tests/stages.rs` was one-sided the other way — it appears only inside
    /// `submit + queue + guest <= observed` — so a `submit_ns` that was
    /// structurally zero passed the whole file, which is how the original
    /// review proved the defect by replacing the computation with a literal
    /// `0`. `queue_ns` had a guard against exactly this and `submit_ns` did
    /// not.
    ///
    /// A bare `> 0` would catch the literal zero and nothing subtler, so this
    /// makes the stage respond to a *cause*: the submit path clones the route
    /// twice and hashes it into the hang breaker's key, all before the stamp,
    /// so a megabyte of route is real work inside the stage and nowhere else.
    /// Requiring the stage to grow with it pins both ends — a stage that is
    /// never stamped cannot grow, and a stage whose opening bracket has
    /// drifted past the work reports it outside `service_ns` and fails
    /// `is_coherent`.
    #[test]
    fn the_submit_stage_responds_to_work_done_inside_it() {
        let cell = Cell::new(
            CellConfig {
                stage_sample_every: 1,
                ..CellConfig::for_test("stage-submit-cause")
            },
            services(),
        );
        let m = app(&cell, "quick", FakeBehaviour::done());
        // The caller's own reading immediately before the call, so the test
        // can see *where* the cell's admission stamp falls relative to the
        // work the submit stage claims to contain.
        let go = |route: String, id: u64| -> (u64, StageTimings) {
            let before = crate::mono_ns();
            let out = cell
                .dispatch_blocking(
                    &m,
                    Target::Handler { route },
                    http(id),
                    Duration::from_secs(30),
                )
                .expect("dispatch");
            (before, out.stages.expect("sampled"))
        };

        // **Minimum over trials, not a single reading.** Both quantities below
        // are wall-clock intervals on a machine running the rest of the test
        // suite, and a preemption inside the short control's submit path
        // inflates it by milliseconds — which is enough to break a ratio
        // against it. The minimum is the least-perturbed sample of a quantity
        // that noise can only add to, and each is minimised independently
        // because they are perturbed independently.
        const TRIALS: u64 = 5;
        let big = format!("/{}", "x".repeat(2 << 20));
        let short = (0..TRIALS)
            .map(|i| go("/go".into(), 1 + i).1.submit_ns)
            .min()
            .unwrap();
        let longs: Vec<(u64, StageTimings)> =
            (0..TRIALS).map(|i| go(big.clone(), 100 + i)).collect();
        let long_submit = longs.iter().map(|(_, s)| s.submit_ns).min().unwrap();
        let gap = longs
            .iter()
            .map(|(before, s)| s.admitted_mono_ns.saturating_sub(*before))
            .min()
            .unwrap();
        let long = longs[0].1;

        assert!(
            long_submit > short * 10,
            "a route two megabytes longer moved submit_ns from {short} ns to {long_submit} ns. \
             The submit path clones that route twice and hashes it before the stamp, so a stage \
             that cannot see the difference is not measuring the submit path"
        );
        // **The bracket opens before the stage, not after it.** This is the
        // assertion that pins `admitted_mono_ns` to admission rather than to
        // whichever stage boundary happens to be nearest: with two megabytes
        // of work inside the submit stage, an admission stamp that had drifted
        // to the *end* of that stage would sit a full `submit_ns` after the
        // caller's own reading. Coherence alone cannot catch that — the cell
        // clones this route again in `run_dispatch` and in `settle`, so the
        // residual grows with it and absorbs the discrepancy.
        assert!(
            gap < long_submit / 2,
            "the cell's admission stamp lands {gap} ns after the caller's own reading, against \
             a submit stage of {long_submit} ns — the opening bracket is sitting after work the \
             stage it opens is supposed to contain"
        );
        assert!(long.is_coherent(), "{long:?}");
        assert!(
            short > 0,
            "even the short route spans a mutex and an allocation"
        );
        cell.shutdown();
    }

    // -----------------------------------------------------------------------
    // the queue stage, across slices
    // -----------------------------------------------------------------------

    /// **A woken slice's wait for a worker is queueing, and it is counted.**
    ///
    /// `queue_ns` was written once, at the top of `run_dispatch`, so it
    /// covered the first admission and nothing else. A woken slice goes
    /// through `run_wake`, which touched no stamp at all — so a request that
    /// parked ten times and waited behind a busy pool on every wake reported
    /// the queueing of its first admission and no more. The wake lane is the
    /// *priority* lane, which makes it the one waiting time most needs to be
    /// visible in.
    ///
    /// One execution thread, and the wake deliberately queued behind a long
    /// dispatch: the parker's own admission was into an empty queue, so
    /// essentially all of its `queue_ns` has to be the wake's wait or the
    /// stage is not looking at the wake lane.
    #[test]
    fn the_queue_stage_covers_the_wake_lane_as_well_as_admission() {
        let cell = Cell::new(
            CellConfig {
                exec_threads: 1,
                max_hot: 1,
                stage_sample_every: 1,
                ..CellConfig::for_test("stage-wake")
            },
            services(),
        );
        // A timer long enough that it never fires on its own, so the test
        // decides when the wake happens.
        let parker = app(
            &cell,
            "parker",
            FakeBehaviour::program([Step::Park {
                imports: vec![("clock_deadline_in", 600_000)],
            }]),
        );
        let slow = app(
            &cell,
            "slow",
            FakeBehaviour::program([Step::Log {
                bytes: 4096,
                times: 60_000,
            }]),
        );

        let route = |p: &str| Target::Handler { route: p.into() };
        let before: Vec<u64> = cell.futures().live_future_ids();
        let parked = cell
            .submit(&parker, route("/park"), http(1))
            .expect("admitted");

        let mut fid = None;
        assert!(
            wait_for(
                || {
                    fid = cell
                        .futures()
                        .live_future_ids()
                        .into_iter()
                        .find(|i| !before.contains(i));
                    fid.is_some()
                },
                Duration::from_secs(10)
            ),
            "the first dispatch never parked, so there is no wake to queue"
        );

        // Occupy the one execution thread, and wait until it is genuinely
        // inside the guest: the wake has to arrive *behind* this, not race it.
        let busy = cell
            .submit(&slow, route("/slow"), http(2))
            .expect("admitted");
        assert!(
            wait_for(|| cell.hot_count() > 0, Duration::from_secs(10)),
            "the long dispatch never reached the guest, so nothing was occupying the pool"
        );

        cell.complete_future(
            fid.unwrap(),
            wisp_abi::result::ResultRecord::Ok(b"v".to_vec()),
        );

        let long = busy
            .recv_timeout(Duration::from_secs(30))
            .expect("the long dispatch never finished")
            .stages
            .expect("sampled");
        let woken = parked
            .recv_timeout(Duration::from_secs(30))
            .expect("the parked dispatch never woke")
            .stages
            .expect("sampled");

        assert!(
            woken.queue_ns >= long.guest_ns / 2,
            "the wake waited for a thread that spent {} ns inside the guest, and the request \
             reports {} ns of queueing — a queue stage that stops at admission cannot see the \
             wake lane at all",
            long.guest_ns,
            woken.queue_ns
        );
        cell.shutdown();
    }

    // -----------------------------------------------------------------------
    // the two panic boundaries
    // -----------------------------------------------------------------------

    /// **A panic inside a dispatch costs that request and nothing else.**
    ///
    /// One execution thread, so there is no second one to hide the loss. The
    /// first dispatch panics; the second is a healthy module on the same
    /// thread. Without the boundary the thread is gone and the second request
    /// waits until its timeout — a cell that silently stops answering, with
    /// nothing in the metrics to say why.
    ///
    /// The panic prints its backtrace to stderr. That is the intended
    /// behaviour and not test noise: a caught host panic is still a host bug
    /// and must stay loud.
    #[test]
    fn a_panicking_dispatch_does_not_take_the_execution_thread_with_it() {
        let cell = Cell::new(
            CellConfig {
                exec_threads: 1,
                max_hot: 1,
                ..CellConfig::for_test("panic-dispatch")
            },
            services(),
        );

        let bad = ModuleId::new("bad", "v1");
        cell.register_module(
            "acme",
            Arc::new(PanicActivator {
                module: bad.clone(),
                template: Arc::new(vec![0u8; wisp_abi::PAGE_SIZE]),
            }),
            manifest("bad"),
        )
        .unwrap();

        let good = app(&cell, "good", FakeBehaviour::done());

        let route = |p: &str| Target::Handler { route: p.into() };
        // Ten seconds is the *bound*, not the expectation: the point is that
        // the caller is answered rather than left to time out. Its responder
        // is in `Cell::active` and survives the unwind, so a cell that only
        // counted the panic would leave this call sitting here for the whole
        // ten seconds and a `submit_with` caller waiting forever.
        let t0 = Instant::now();
        let failed = cell
            .dispatch_blocking(&bad, route("/go"), http(1), Duration::from_secs(10))
            .expect("the caller must be answered, not left waiting");
        assert_eq!(failed.status, 500, "a host fault is a 500: {failed:?}");
        assert_eq!(
            failed.trap.as_deref(),
            Some("host_panic"),
            "and it says what happened"
        );
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "the answer took {:?}, which is a timeout wearing a status code",
            t0.elapsed()
        );

        let ok = cell
            .dispatch_blocking(&good, route("/go"), http(2), Duration::from_secs(10))
            .expect("the pool's only thread must still be running");
        assert!(
            (200..300).contains(&ok.status),
            "the next request is served normally: {ok:?}"
        );

        assert_eq!(
            cell.caught_panics(),
            1,
            "exactly one panic, counted — a contained fault that is not counted is a \
             silently slower cell"
        );
        cell.shutdown();
    }

    /// **A stale panic handler does not answer somebody else's request.**
    ///
    /// The window is real and was introduced by the containment fix itself:
    /// `finish_and_release` removes the `active` entry and returns the
    /// instance to its pool *before* the job returns, so a worker that panics
    /// in the tail after that point is holding an instance id another worker
    /// may already have acquired and filled. Answering on the id alone failed
    /// that worker's caller with a `host_panic` it had nothing to do with —
    /// one broken request turned into two, and the second one was healthy.
    ///
    /// Reproducing the interleaving by racing threads would be a sampler.
    /// Instead this constructs the *state* the race produces — a live request
    /// on an instance, and a handler holding a stale dispatch id for it — and
    /// calls the handler directly, which is deterministic and tests the guard
    /// rather than the scheduler. Both directions are asserted: a stale id
    /// must not answer, and the matching id must, or the "guard" would be
    /// satisfied by a handler that never answers anything.
    #[test]
    fn a_stale_panic_handler_does_not_answer_another_requests_caller() {
        let cell = Cell::new(
            CellConfig {
                exec_threads: 2,
                ..CellConfig::for_test("stale-panic")
            },
            services(),
        );
        // Parks on a timer that will not fire, so the request stays live and
        // answerable for as long as the test needs it.
        let m = app(
            &cell,
            "parker",
            FakeBehaviour::program([Step::Park {
                imports: vec![("clock_deadline_in", 600_000)],
            }]),
        );
        let target = Target::Handler { route: "/".into() };

        const LIVE: u64 = 7;
        let rx = cell.submit(&m, target, http(LIVE)).expect("admitted");
        assert!(
            wait_for(
                || !cell.residents.lock().is_empty(),
                Duration::from_secs(10)
            ),
            "the Handler instance never became resident"
        );
        let id = {
            let residents = cell.residents.lock();
            assert_eq!(residents.len(), 1, "the single parked Handler is resident");
            *residents
                .keys()
                .next()
                .expect("the resident count was checked")
        };
        assert!(
            wait_for(
                || cell.active.with(&id, |a| a.dispatch_id) == Some(LIVE),
                Duration::from_secs(10)
            ),
            "dispatch {LIVE} never reached `active`"
        );

        // A worker that panicked while holding this instance for an *earlier*
        // dispatch. On the identity alone this answers the live request.
        cell.answer_after_panic(Some((id, LIVE - 1)));
        assert!(
            matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "a handler holding a stale dispatch id answered the live request's caller"
        );
        assert_eq!(
            cell.active.with(&id, |a| a.dispatch_id),
            Some(LIVE),
            "and it deleted the live request's entry on the way past"
        );

        // The matching id must still answer, or the guard is just a mute.
        cell.answer_after_panic(Some((id, LIVE)));
        let out = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the owning handler must answer");
        assert_eq!(out.status, 500);
        assert_eq!(out.trap.as_deref(), Some("host_panic"));
        assert_eq!(out.dispatch_id, LIVE);
        cell.shutdown();
    }
}
