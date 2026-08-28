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

use crate::marks::{self, Mark, RequestMarks};

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
    /// How densely a request's [`crate::marks`] trace is collected: `N` times
    /// one request in `N`, `0` or `1` — `1` meaning every request — with `0`
    /// switching it off.
    ///
    /// **Sampled rather than always-on because the instrument is the cost.**
    /// At 300,000 requests a second, tracing every one would be 0.5 s of
    /// `RDTSCP` per second across the pool — on the one path whose timing is
    /// the measurement.
    ///
    /// **Eighty-odd counter reads, not three clock calls.** The mechanism is
    /// no longer `clock_gettime`: a mark is one `RDTSCP`, 20 ns on the part
    /// this suite targets, and a plain request takes about eighty of them —
    /// roughly 1.6 µs of instrument. A request that **parks** takes more, once
    /// per pass. All of it is conditional on this sample.
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

/// **Not `Clone`.** It carries the request's marks, which are a checked-out
/// buffer with exactly one owner: a second copy would either duplicate a
/// pooled resource or silently drop the trace, and both are worse than the
/// clone being unavailable. Nothing in this workspace cloned one.
#[derive(Debug)]
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
    /// The request's labelled timestamps, if it was one of the traced ones.
    ///
    /// Rides home for the same reason `stages` does, and returns itself to
    /// [`crate::marks::RequestMarksPool`] when the caller drops it — so a
    /// caller that never reads the trace still gives the buffer back rather
    /// than starving the pool.
    ///
    /// `None` on every untraced request, on every request that arrived when
    /// the pool was dry, and on every refusal: a shed or a breaker rejection
    /// never reaches most of the marks, and a partial trace pooled with whole
    /// ones would shorten every span it appears in.
    pub marks: Option<RequestMarks>,
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
            marks: None,
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
            marks: None,
        }
    }
}

// ---------------------------------------------------------------------------
// The work queues
// ---------------------------------------------------------------------------

/// **Does this outcome end in a tombstone?**
///
/// Asked while the sandbox lock is still held, because the answer decides
/// whether the guard is *released* or *destroyed* — and those differ in
/// ordering, which is the whole point. `CallGuard::destroy` sets the tombstone
/// and then clears `in_call`; plain `Drop` clears `in_call` and leaves the
/// tombstone to `settle`, hundreds of lines later.
///
/// **That gap was a real hole.** Between the guard dropping and `settle`
/// tombstoning, `Slot::enter` still admitted callers: a second completion
/// queued for the same instance would be taken by another execution thread,
/// which would then run guest code on memory a trap abandoned mid-flight. The
/// two `settle`s then raced, and the loser resurrected the tier from
/// `Discarded` back to `Parked` permanently. Reproduced at 4 runs in 20 on an
/// *idle* machine by `dispatcher.rs`'s reentrancy test, which is why that test
/// was thought merely flaky. `instances.rs`'s claim that "the window between
/// 'it was hard-killed' and 'its memory is gone' is not a window at all" was
/// true only of the hibernation path, which is the one caller that already
/// tombstoned first.
///
/// The latched case is included and is not decoration: `settle` promotes a
/// latched trap into `Completion::Trapped` a few lines below, so a `Done` whose
/// vtable owed the guest a trap tombstones exactly like an outright one — and
/// asking `take_trap` here instead would consume the latch that promotion needs.
fn ends_in_tombstone(out: &DispatchOutcome, slot: &Arc<Slot>) -> bool {
    matches!(out.completion, Completion::Trapped | Completion::BudgetKill)
        || (matches!(out.completion, Completion::Done | Completion::Parked) && slot.ctx.has_trap())
}

/// What a submission did, and what it handed back.
///
/// [`Cell::submit_with_marks`] cannot answer with a bool alone once the caller
/// owns the trace buffer: a shed sends nothing on the reply channel —
/// deliberately, so a caller draining one shared receiver cannot count the same
/// refusal twice — so the return value is the only route a shed's trace has
/// home.
pub struct SubmitOutcome {
    /// `false` if the cell shed at admission (§6.4).
    pub admitted: bool,
    /// The trace of a **shed** request, handed back so the caller can close and
    /// record it. `None` when the request was admitted (the cell kept it) or
    /// when there was no trace.
    pub refused_marks: Option<RequestMarks>,
}

enum Job {
    Dispatch(Box<DispatchJob>),
    /// A host future completed for this instance; deliver it.
    Wake(WakeItem),
}

/// A queued wake, and when it was queued.
///
/// The reading is taken here rather than at the pop because the interval
/// wanted is the one *before* the pop — a woken slice waits for an execution
/// thread exactly as a new dispatch does, and §6.3 makes both of them the same
/// request. See [`crate::marks::Mark::WakeStarted`].
struct WakeItem {
    slot: Arc<Slot>,
    /// **Everything the completion path measured, on its way to a worker that
    /// can file it.**
    ///
    /// `Some` only for a request that [`CellConfig::stage_sample_every`]
    /// selected, so an unsampled wake carries eight bytes and a `None`.
    ///
    /// Not the trace itself: a parked request's trace lives in `Cell::active`,
    /// and no thread on this path can reach it — the reactor completing the
    /// future, whoever pushes the wake, the worker that pops it. Taking
    /// `Cell::active` on the wake path in order to measure the wake path is
    /// self-defeating, so each event's reading is taken where it happens, as
    /// plain integers, and the whole carrier is filed by the worker that picks
    /// the wake up. See [`crate::marks::WakeStamps`].
    ///
    /// Behind an `Arc` for two reasons. There are ten readings and this rides
    /// inside a `Job`, which every dispatch is moved through as well, so inline
    /// it would make the dispatch path pay 160 bytes a move to measure the wake
    /// path. And the producer keeps a handle across the push, because pushing
    /// is a handoff and it still has a lock to release and a worker to wake.
    stamps: Option<Arc<crate::marks::WakeStamps>>,
    /// **The dispatch this wake belongs to**, which is not the same thing as
    /// the instance it is addressed to.
    ///
    /// A wake is routed by instance id, because that is what owns the future
    /// table and the slot. But the readings above were taken on behalf of the
    /// request that *awaited* the completion, and `Cell::active` holds one
    /// entry per instance — so if that request has finished and another has
    /// begun on the same instance by the time a worker pops this, the trace
    /// found there belongs to somebody else. Comparing this against
    /// `Slot::ctx`'s current dispatch id is what tells the two apart.
    ///
    /// Set from the **future's** dispatch on the completion path
    /// (`CellRuntime::settle_future`), and from the slot's current dispatch on
    /// the cell's own re-queue — see `Cell::queue_wake` for why those are both
    /// the same rule rather than two.
    ///
    /// `None` is "no dispatch was named", which only [`WakeSink::signal`] can
    /// produce — the tracing-free entry point, which no caller in this crate
    /// uses. It is judged **fresh**: a guard that cannot tell must not refuse,
    /// or a sink that opted out of the instrument would silently lose traces
    /// to it.
    dispatch: Option<u64>,
}

struct DispatchJob {
    module: ModuleId,
    target: Target,
    rec: DispatchRecord,
    responder: std::sync::mpsc::SyncSender<DispatchResult>,
    enqueued_ns: u64,
    /// The request's marks while it is queued.
    ///
    /// **On the job, not beside it**, because the job is the thing that has
    /// exactly one owner at every instant: the producer holds it until the
    /// enqueue, the queue holds it until the pop, and the worker holds it
    /// after. `Marks` is single-owner, and this is the only field in the cell
    /// with the same lifetime.
    marks: Option<RequestMarks>,
    /// **The instant this job was handed to a lane**, or
    /// [`marks::NO_STAMP`] when it is untraced.
    ///
    /// Written by the producer *before* the push — so it is not a handoff and
    /// needs no shared storage — and read by the consumer, which is the only
    /// party with a use for it. It exists for one comparison: a worker's
    /// `Condvar::wait` reading is this job's wake-up only if this job was
    /// already queued when the worker woke, and under a lock-free ring that is
    /// not something the consumer can otherwise know. See
    /// [`marks::woke_belongs`] and `WorkQueue::stamp_taken`.
    pushed: crate::marks::Stamp,
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
        f.dispatches.pop().map(|d| {
            // The wait closes at the pop, on the thread that now owns the job.
            // Its own label, not the mutex lane's: under a ring this span also
            // contains the push and the notify, because a ring's producer has
            // no instant after the push at which it still owns the job. See
            // `Mark::RingTaken`.
            marks::mark(&d.marks, Mark::RingTaken);
            Job::Dispatch(d)
        })
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

    /// `Err` means shed (§6.4).
    fn push_dispatch(&self, j: Box<DispatchJob>) -> Result<(), Box<DispatchJob>> {
        let j = j;
        if let Some(f) = &self.free {
            // There is no placement to make here — one ring — so the boundary
            // is simply "before the enqueue", above the `pushing` bookkeeping
            // as well as above the compare-and-swap. See `stamp_queued`.
            //
            // The mark is emitted all the same, so the span that ends at it
            // means the same thing under every structure: everything the
            // producer did between building the job and beginning the enqueue.
            // Under one ring that is `pick`'s early return and nothing else,
            // which is the honest reading rather than a missing row.
            marks::mark(&j.marks, Mark::PlacementDecided);
            // Announce before the check, release after the push. See
            // `LockFreeLanes::pushing`: without this pair the check and the
            // push are two steps and a job can land in a ring that every
            // worker has already left.
            f.pushing.fetch_add(1, Ordering::SeqCst);
            marks::mark(&j.marks, Mark::PushRegistered);
            let closed = f.closed.load(Ordering::SeqCst);
            marks::mark(&j.marks, Mark::ClosedChecked);
            if closed {
                f.pushing.fetch_sub(1, Ordering::SeqCst);
                return Err(j);
            }
            // **Nothing is marked after the push, and nothing can be.** The
            // ring takes the job by value and the `pushing` guard has to be
            // released afterwards, so there is no instant at which this thread
            // still owns the job and has work left to record. The mutex arm
            // answers that with a `MarkWriter` held across the handoff; here
            // the mark it would take races the consumer, and a mark that
            // arrives out of order does not go missing — it renames the pair it
            // falls between.
            //
            // So the compare-and-swap, the `SeqCst` decrement and the notify
            // all sit inside the span the *consumer* closes, under
            // `Mark::RingTaken`, which is a different label from the mutex
            // lane's `queue.wait` for exactly that reason. This is the same
            // trade `push_wake` makes, and it is made in the same place.
            //
            // Stamped before the push, and only when this job is traced. Not a
            // mark: nothing in the report closes a span here. It is the left
            // edge the consumer needs to decide whether its own `Condvar::wait`
            // reading belongs to this job. See `marks::woke_belongs`.
            let mut j = j;
            if j.marks.is_some() {
                j.pushed = marks::stamp();
            }
            // A full ring hands the job straight back, which is exactly the
            // shed the mutex version does at `dispatches.len() >= cap`. That
            // shed is decided inside the same span, and closes at
            // `Mark::ShedDecided`.
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
        marks::mark(&j.marks, Mark::PlacementDecided);
        // **The two marks this whole exercise exists for.** Everything between
        // them is the wait for the lane's mutex and nothing else — no
        // placement above it, no queueing below it. The old `queue_ns`
        // contained both this wait and the wait in the lane for a worker, and
        // those have different fixes: a lock wait wants a shorter critical
        // section or more lanes, a queue wait wants more workers.
        //
        marks::mark(&j.marks, Mark::LaneLockWanted);
        let mut g = lane.inner.lock();
        marks::mark(&j.marks, Mark::LaneLockAcquired);
        if g.closed || g.dispatches.len() >= self.cap {
            return Err(j);
        }
        // **Taken before the job is given away, used after.** The push moves
        // the job into a queue a worker may pop on the next instruction, so
        // from here this thread has no job to mark through — but it does still
        // have work to do, and that work is on the request's critical path.
        // See `crate::trace` for why the storage is shared.
        let tail = marks::writer(&j.marks);
        // See the lock-free arm: taken before the push, for the consumer.
        let mut j = j;
        if j.marks.is_some() {
            j.pushed = marks::stamp();
        }
        g.dispatches.push_back(j);
        lane.load.fetch_add(1, Ordering::Relaxed);
        drop(g);
        // The critical section: a `len()` check, a `push_back` and a
        // `fetch_add`, about 4 ns — below the counter's own 10 ns step, so
        // this row is expected to read as noise. It is here for what it does
        // to its neighbour: closing the lock *here* is what makes `queue.wait`
        // the pure wait for a worker rather than that wait plus a producer's
        // tail.
        marks::mark_after(&tail, Mark::LaneLockReleased);
        // **Nothing is read after this call**, for the reason `push_wake` gives
        // at the same line. `notify_one` wakes a thread that starts running
        // while this one is still returning from the syscall, and that thread
        // can pop the job and mark `JobTaken` before a reading taken on the
        // next line lands. Sometimes that reading is simply lost; sometimes it
        // lands *after* its neighbour, which is worse, because the trace is one
        // timeline sorted by timestamp and every span is named after the mark
        // that ends it — so a late mark renames the pair it falls between,
        // cutting `queue.wait` on one request and `worker.start` on the next
        // with nothing in the report to say which.
        //
        // The consumer takes it instead, on its first instruction out of
        // `Condvar::wait`. That has no race at all — the thread that reads the
        // counter is the thread that files it — and it is the better quantity:
        // what this thread pays for the syscall is the *submitter's* cost and
        // overlaps the request's timeline without gating it, where the wait
        // until a worker is actually running is what gates it. See
        // `Mark::WorkerWoken`, which is one row over both job kinds because
        // both park a worker and both wake one.
        lane.cv.notify_one();
        Ok(())
    }

    /// A wake is never shed, so it is placed regardless of capacity.
    ///
    /// It counts toward the lane's load all the same: a lane holding four
    /// hundred wakes is the busiest lane in the cell, and a placer that could
    /// not see them was choosing it on purpose.
    fn push_wake(&self, item: WakeItem) {
        // **Kept across the handoff.** The push moves the item into a queue a
        // worker may pop on the next instruction, and this thread still has to
        // release the lane lock and wake a worker — a futex call, and the
        // largest thing on the dispatch side of the same sequence. The carrier
        // is shared for exactly the reason `crate::trace` is: see
        // `marks::WakeStamps`.
        let tail = item.stamps.clone();
        if let Some(f) = &self.free {
            // **The ring's push and the wait for a consumer cannot be told
            // apart here**, and the reason is the handoff: the readings ride
            // *on* the item, and `push` takes the item by value, so the instant
            // the compare-and-swap lands this thread has nothing left to write
            // into. The dispatch path solves the same problem with a
            // `MarkWriter` — a second handle on the request's trace — and a
            // reactor thread has no trace to take one from, which is the whole
            // reason these readings are carried at all. So `taken` closes a
            // span that contains the push as well as the wait, under
            // `Mark::WakeRingTaken`, which is a different label from the mutex
            // lane's for exactly that reason.
            if let Some(st) = &tail {
                st.ring();
            }
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
        // A **triple**, and the lane's own hold is expected to read as noise —
        // a `closed` check, a `push_back` and a `fetch_add`, about 4 ns against
        // a 10 ns counter step. It is here for what it does to its neighbour:
        // closing the lock at this line is what makes `wake.queue.wait` the
        // pure wait for a worker rather than that wait plus a producer's tail.
        // Exactly the argument `push_dispatch` makes for the dispatch lane.
        if let Some(st) = &tail {
            st.lane_wanted.take();
        }
        let lane = &self.lanes[self.pick()];
        let mut g = lane.inner.lock();
        if let Some(st) = &tail {
            st.lane_acquired.take();
        }
        if g.closed {
            return;
        }
        g.wakes.push_back(item);
        lane.load.fetch_add(1, Ordering::Relaxed);
        drop(g);
        if let Some(st) = &tail {
            st.lane_released.take();
        }
        // **Nothing is read after this call.** The futex round trip is worth
        // its own span — on the dispatch side of the same sequence it reads
        // 3.8 µs at the median, which is far too large to hide inside a queue
        // wait — but it is not measurable from *here*. `notify_one` wakes a
        // thread that starts running while this one is still returning from the
        // syscall, and that thread can pop the item, reclaim the trace and file
        // the carrier before a reading taken on the next line lands. The
        // readings lost that way are the slow notifies, which is the worst
        // possible bias for the row.
        //
        // So the consumer takes it instead, on its first instruction out of
        // `Condvar::wait` — no race, and a better quantity: what this thread
        // pays for the syscall is the *reactor's* cost and overlaps the
        // request's timeline without gating it, where the wait for a worker to
        // actually be running is what gates it. See `Mark::WorkerWoken`.
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
        // **Set only when this thread actually had to be woken**, and cleared
        // by `pop` returning: a worker already looping never reaches the
        // condvar, and for that wake there is no futex round trip to report.
        // See `Mark::WorkerWoken`.
        let mut woke: Option<crate::marks::Stamp> = None;
        if let Some(f) = &self.free {
            loop {
                // Fast path: no lock, no atomics beyond the queue's own.
                if let Some(j) = Self::free_try_pop(f) {
                    return Some(Self::stamp_taken(j, woke));
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
                    if let Some(j) = missed {
                        return Some(Self::stamp_taken(j, woke));
                    }
                    return None;
                }
                f.cv.wait(&mut g);
                // First instruction after the wait: this thread is awake
                // because a producer's notify reached it. Overwritten on a
                // second pass, so a spurious wake-up does not leave a stale
                // reading for whatever is popped later.
                woke = Some(marks::stamp());
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
                // **The lane wait closes here**, while the lane lock is still
                // held, for the same reason `JobTaken` does on the dispatch
                // side: this is the first instant this thread owns the item.
                // Free for an untraced wake — one `Option` check.
                return Some(Self::stamp_taken(Job::Wake(s), woke));
            }
            if let Some(d) = g.dispatches.pop_front() {
                // **The queue wait closes here**, while the lane lock is still
                // held — which is where it has to close, because this is the
                // first instant this thread owns the job. The span running
                // back to `LaneLockAcquired` is the producer's critical
                // section, the notify, the wait in the lane, and this thread's
                // own acquisition. Everything that stretches it is a worker
                // being busy.
                marks::mark(&d.marks, Mark::JobTaken);
                return Some(Self::stamp_taken(Job::Dispatch(d), woke));
            }
            if g.closed {
                return None;
            }
            lane.cv.wait(&mut g);
            // See the note on `woke` above: taken here, on the way out of the
            // wait and with the lane lock already back in hand, because a
            // producer cannot measure its own notify without racing the thread
            // it wakes.
            woke = Some(marks::stamp());
        }
    }

    /// Close a traced wake's lane wait at the instant a consumer owns it, and
    /// file the wake-up this thread was given if it had to be woken at all.
    ///
    /// One place rather than four: `pop` has four returns that can hand back an
    /// item, and a reading taken at only some of them would make the rest
    /// report a lane wait that ran to the *next* mark.
    ///
    /// `woke` is the consumer's own reading of when it came out of
    /// `Condvar::wait`, which is where the futex round trip has to be measured
    /// — a producer cannot time its own `notify_one` without racing the thread
    /// it wakes, and losing the slow ones. `None` when this worker was already
    /// running, which is not a lost observation: there was no round trip.
    #[inline]
    fn stamp_taken(j: Job, woke: Option<crate::marks::Stamp>) -> Job {
        match &j {
            Job::Wake(w) => {
                if let Some(st) = &w.stamps {
                    // Guarded exactly as the dispatch arm below is, against the
                    // last reading the producer took before letting the item go.
                    // The carrier always has one — both push sites stamp
                    // `arrived` — so this refuses a reading rather than every
                    // reading.
                    if let Some(at) = woke {
                        if marks::woke_belongs(at, st.enqueued_at()) {
                            st.woke.set(at);
                        }
                    }
                    st.taken.take();
                }
            }
            // **A dispatch carries its own trace**, so the reading goes
            // straight into it rather than onto a carrier: there is no handoff
            // left to cross, this thread owns the job.
            //
            // Guarded the same way, against `pushed`: the instant the
            // producer handed this job to a lane. The comparison asks the only
            // question that matters — was this job
            // already queued when this worker woke? Under a mutex lane it
            // always was, because the wait returns holding the lane lock and
            // the pop happens before it is released. Under a ring it need not
            // have been, and filing the reading then would put it in front of
            // the job's own marks and rename them. See `marks::woke_belongs`.
            Job::Dispatch(d) => {
                if let Some(at) = woke {
                    if marks::woke_belongs(at, d.pushed) {
                        marks::mark_at(&d.marks, Mark::WorkerWoken, at);
                    }
                }
            }
        }
        j
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

    /// What each lane still owes, in the order [`WorkQueue::pop`] serves them:
    /// lane `i` is worker `i`'s, and no worker steals.
    ///
    /// [`Lane::load`], not queue length — it counts the job a worker has taken
    /// and not finished as well as everything still queued, which is the
    /// difference that makes it usable as a "this lane is occupied" signal.
    /// Empty under `LockFree`, which has no lanes.
    fn lane_loads(&self) -> Vec<usize> {
        self.lanes
            .iter()
            .map(|l| l.load.load(Ordering::Relaxed))
            .collect()
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
    started_ns: u64,
    cpu_ns: u64,
    wall_ns: u64,
    slices: u32,
    acquired: &'static str,
    budget: CpuBudget,
    /// **Where the trace lives while the request is parked.**
    ///
    /// It is a local on the execution thread for as long as one is running the
    /// request, because a local needs no lock. A parked request has no thread,
    /// so between the park and the wake it lives here — reclaimed by whichever
    /// worker picks the wake up. `None` while a thread holds it, which is the
    /// common case and is why nothing reads this on the plain path.
    marks: Option<RequestMarks>,
}

thread_local! {
    /// **The tail this worker is measuring**, between the end of
    /// `Cell::finish_and_release` and the top of the next `pop`.
    ///
    /// A thread-local rather than a parameter because the two ends of the tail
    /// are in different functions with a `catch_unwind` between them:
    /// `finish_and_release` opens it, four frames down inside `settle`, and
    /// `exec_loop` closes it after `job_done`. Threading a buffer back up
    /// through `settle`'s `bool` return would put the tail's storage in the
    /// same variable as the request's, and the first mistake anybody made with
    /// that would be a tail span filed into a request's trace — the one
    /// outcome the whole separation exists to prevent.
    ///
    /// `None` on every job that was not sampled and on every job that parked,
    /// so the common path is one TLS read and an `Option` check.
    static TAIL: std::cell::RefCell<Option<RequestMarks>> =
        const { std::cell::RefCell::new(None) };
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
        self.signal_untraced(owner)
    }

    fn signal_traced(
        &self,
        owner: InstanceId,
        dispatch: u64,
        stamps: Option<Arc<marks::WakeStamps>>,
    ) {
        self.push(owner, Some(dispatch), stamps)
    }
}

impl CellWake {
    /// The [`WakeSink::signal`] path: a completion with no dispatch named and
    /// nothing measured. Nothing in this crate reaches it — every completion
    /// goes through `CellRuntime::settle_future` — but it is public API and it
    /// has to route the wake, so it pushes one that names no dispatch.
    fn signal_untraced(&self, owner: InstanceId) {
        self.push(owner, None, None)
    }

    fn push(
        &self,
        owner: InstanceId,
        dispatch: Option<u64>,
        stamps: Option<Arc<marks::WakeStamps>>,
    ) {
        // The index is a **pair**: the hold is one hash, one lookup and a
        // `Weak::upgrade`, at about what a mark costs. Every completion in the
        // cell passes through this one mutex and it appears in no row of
        // `Cell::lock_stats`.
        if let Some(st) = &stamps {
            st.index_wanted.take();
        }
        let slot = self.index.lock().get(&owner).and_then(Weak::upgrade);
        if let Some(st) = &stamps {
            st.index_released.take();
        }
        let Some(s) = slot else {
            // **The completion is already in the future table** — `complete`
            // wrote it Ready and pushed it onto the owner's `ready` queue
            // before this was called — and this is the only thing that would
            // have told a worker to come and get it. Dropping it here means
            // the result stays queued against an instance nobody will visit
            // again and the request that awaited it is never answered.
            //
            // For a genuinely discarded instance that is right: §6.1 holds
            // only `Weak` references precisely so a late completion is a no-op
            // rather than a resurrection. For a live one it is a lost request.
            // Nothing distinguished the two, and nothing counted either, so
            // the symptom was a request that simply stopped — `dispatches`
            // still claiming it was in flight, no trap, no refusal, no log.
            self.metrics.wakes_dropped.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                instance = %owner,
                "a completion arrived for an instance that is no longer resident; the wake \
                 is dropped and any dispatch awaiting it will not be answered (§6.1)"
            );
            return;
        };
        self.metrics.wakes.fetch_add(1, Ordering::Relaxed);
        self.queue.push_wake(WakeItem {
            slot: s,
            stamps,
            dispatch,
        });
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
    /// **Buffers and a sink for the cell's own tails**, or `None` in every
    /// embedding that does not trace.
    ///
    /// Read behind the sampling predicate and nowhere else, so an untraced
    /// dispatch never touches this lock: `stage_sampled` is an atomic load and
    /// a modulo, and it is false for 127 jobs in 128 before anything here is
    /// looked at. On the one that passes, a read guard is a few nanoseconds
    /// against a tail that is about to take fifteen timestamps.
    ///
    /// Installed after construction rather than passed to [`Cell::new`]: the
    /// exec pool starts inside the constructor, and a `Calibration` costs a
    /// hundred milliseconds to measure and belongs to the harness that is
    /// already holding one.
    tails: parking_lot::RwLock<Option<Arc<crate::marks::CellTails>>>,
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
            }),
            reactor: runtime.handle().clone(),
            stage_sample_every: cfg.stage_sample_every,
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
            tails: parking_lot::RwLock::new(None),
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
                    Job::Wake(w) => cell.run_wake(w.slot, w.stamps, w.dispatch),
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
            // **After `job_done`, and on every job.** The span this closes is
            // the job's unwind, the in-flight slot being cleared and the
            // lane's load being released — everything between the cell having
            // finished with the request and this worker being able to take
            // another. A job that parked or was never sampled has no tail
            // stashed and this is a thread-local read and an `Option` check.
            if let Some(cell) = &cell {
                cell.tail_close();
            }
            if cell.is_none() {
                return;
            }
        }
    }

    /// **Install buffers and a sink for the cell's own tails.**
    ///
    /// What the cell does after answering — the outbox discard, the `active`
    /// removal, reaping the dispatch's futures, the whole-table scan that
    /// releases the instance's handles, the return to the pool — holds an
    /// execution thread and is therefore capacity. It is *not* the request's
    /// latency, because the caller already has its answer, and it cannot go on
    /// the request's trace in any case: that buffer travels inside the reply.
    ///
    /// So it goes on a trace of its own, into a sink of its own, under labels
    /// no request-side row shares. See `crate::marks` on where the trace ends.
    pub fn set_tail_traces(&self, tails: crate::marks::CellTails) {
        *self.tails.write() = Some(Arc::new(tails));
    }

    /// How many execution threads this cell is running, for sizing a tail
    /// pool: a worker holds one buffer from `finish_and_release` until it
    /// takes its next job, so the peak is one per thread.
    pub fn exec_threads(&self) -> usize {
        self.cfg.exec_threads
    }

    /// Open a tail trace, if this job's dispatch is one of the sampled ones
    /// and the harness gave the cell somewhere to put it.
    ///
    /// The predicate is the same one the request's own trace was selected by,
    /// on the same dispatch id, so a run's tails and its requests are the same
    /// sample and the two tables rest on the same population.
    fn tail_begin(&self, slot: &Arc<Slot>) -> Option<RequestMarks> {
        if !stage_sampled(slot.ctx.dispatch_id(), self.cfg.stage_sample_every) {
            return None;
        }
        let tails = self.tails.read().clone()?;
        let t = tails.pool.checkout()?;
        t.begin();
        Some(t)
    }

    /// Park the tail on the worker's thread-local so `exec_loop` can close it.
    fn tail_stash(t: Option<RequestMarks>) {
        if t.is_some() {
            TAIL.with(|c| *c.borrow_mut() = t);
        }
    }

    /// **Close the tail and hand it over.** Called after `job_done` on every
    /// job, so nothing can be left behind by a job that parked, panicked or
    /// was never sampled.
    ///
    /// `JobDone` is taken *here* rather than at the end of
    /// `finish_and_release`, so the span it closes is the job's own unwind,
    /// the in-flight slot being cleared and the lane's load being released —
    /// the last thing standing between this worker and the next request.
    ///
    /// Reducing the trace and handing it to the sink is real work on an
    /// execution thread, and it is deliberately *after* the last mark, so it
    /// is inside no span. At one job in `stage_sample_every` it is a sort of
    /// fifteen entries per hundred-odd dispatches.
    fn tail_close(&self) {
        let t: Option<RequestMarks> = TAIL.with(|c| c.borrow_mut().take());
        if t.is_none() {
            return;
        }
        marks::mark(&t, Mark::JobDone);
        let sink = self.tails.read().as_ref().map(|c| c.sink.clone());
        if let (Some(sink), Some(t)) = (sink, t.as_ref()) {
            sink.record(&marks::read(t));
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

    /// How densely this cell is tracing requests. See [`crate::marks`].
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

    /// Returns the configured hot-dispatch concurrency value.
    pub fn max_hot(&self) -> usize {
        self.cfg.max_hot
    }

    /// Which structure `Cell::active` is, read from the map rather than from
    /// the config — the same reason [`Cell::queue_structure`] asks the queue.
    pub fn active_map(&self) -> crate::lockstats::ActiveMapKind {
        self.active.kind()
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
        //
        // Rule 2 applies under `LockFree`, which has no such lock to report:
        // its lookups take none at all and its per-entry mutexes are
        // uncounted, so a row here would be a zero standing where a
        // measurement goes. See `LockFreeMap::lock_stats`.
        let active_row = match self.active.kind() {
            crate::lockstats::ActiveMapKind::Plain => Some("Cell::active[plain]"),
            crate::lockstats::ActiveMapKind::Sharded => Some("Cell::active[sharded]"),
            crate::lockstats::ActiveMapKind::LockFree => None,
        };
        let mut rows = vec![("FutureTable::inner", self.rt.futures.lock_stats())];
        if let Some(name) = active_row {
            rows.insert(0, (name, self.active.lock_stats()));
        }
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
    /// What each lane still owes, worker by worker.
    ///
    /// Coarser than [`Cell::queue_depth`] on purpose: a lane's load includes
    /// the job its worker is *running*, so a zero here means that worker has
    /// finished everything it was given rather than merely that its queue is
    /// empty. That distinction is the only way to tell, from outside, that a
    /// dispatch has run to the end of `run_dispatch` and not just to the end
    /// of the guest — which is what a test placing work into a specific lane
    /// has to know before it counts on where the next push lands.
    ///
    /// Empty under `LockFree`: there are no lanes to report.
    pub fn lane_loads(&self) -> Vec<usize> {
        self.queue.lane_loads()
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
        let enqueued_ns = crate::mono_ns();
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
            enqueued_ns,
            // `dispatch_blocking` is the one-thread-per-request form used by
            // tests and the control plane, never by the load generator, so it
            // has no pool to check a buffer out of and carries no trace.
            marks: None,
            // Filled by `WorkQueue::push_dispatch`, and only on a traced job.
            pushed: marks::NO_STAMP,
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
        self.submit_with_marks(module, target, rec, responder, None)
            .admitted
    }

    /// [`Cell::submit_with`], carrying a request's marks in with it.
    ///
    /// **The caller checks the buffer out, not the cell.** The first mark of a
    /// request is the caller releasing it, which happens before the cell is
    /// entered at all — so a cell-side checkout could not take it, and the
    /// span the report most needs (`caller.released` → `cell.entered`, the
    /// call into the cell) would not exist. The pool lives with whoever is
    /// generating load; the cell only ever holds a buffer it was handed.
    ///
    /// A refused request's marks are dropped here rather than returned to the
    /// caller. That returns the buffer to the pool — the guard does it on
    /// drop — and loses the partial trace, which is deliberate: a shed reaches
    /// perhaps six of a request's fifty marks, and pooling that into the same
    /// distributions as a served request would shorten every one of them.
    pub fn submit_with_marks(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        responder: std::sync::mpsc::SyncSender<DispatchResult>,
        marks: Option<RequestMarks>,
    ) -> SubmitOutcome {
        marks::mark(&marks, Mark::CellEntered);
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
        let enqueued_ns = crate::mono_ns();
        let now = crate::now_ms();
        let dispatch_id = rec.dispatch_id;
        let key = BreakerKey {
            module: module.clone(),
            route: target.route(),
        };
        // A **triple**. The hold is ~34 ns of hashing three strings against a
        // mark that costs 20 ns here, so the wait for `entries` and the work
        // under it separate cleanly. It was briefly a pair, on the strength of
        // a calibration that measured the mark cost on a core fresh out of a
        // sleep and reported double.
        marks::mark(&marks, Mark::BreakerLockWanted);
        let mut marks = marks;
        let refused = self.breaker.check_marked(&key, now, &mut marks).is_err();
        marks::mark(&marks, Mark::BreakerChecked);
        if refused {
            self.metrics
                .breaker_rejections
                .fetch_add(1, Ordering::Relaxed);
            marks::mark(&marks, Mark::BreakerRefused);
            // The refusal carries the trace home the same way a completion
            // does: the caller waited for this answer and the wait is real.
            let mut r = DispatchResult::breaker_open(dispatch_id);
            r.marks = marks;
            marks::mark(&r.marks, Mark::ReplyHandedOff);
            let _ = responder.send(r);
            return SubmitOutcome {
                admitted: true,
                refused_marks: None,
            };
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder,
            enqueued_ns,
            marks,
            // Filled by `WorkQueue::push_dispatch`, immediately before the job
            // is handed to a lane. See `DispatchJob::pushed`.
            pushed: marks::NO_STAMP,
        });
        marks::mark(&job.marks, Mark::JobBuilt);
        match self.queue.push_dispatch(job) {
            Ok(()) => SubmitOutcome {
                admitted: true,
                refused_marks: None,
            },
            Err(mut j) => {
                // **The shed's trace goes back to the caller**, not into the
                // reply channel: `submit_with` deliberately sends nothing on a
                // shed, because a caller draining one shared receiver cannot
                // tell a synthesised refusal from a completion and would count
                // it twice. So the buffer is handed back through the return
                // value, which is the only channel a shed has.
                marks::mark(&j.marks, Mark::ShedDecided);
                self.metrics.shed.fetch_add(1, Ordering::Relaxed);
                // **A refusal is not free, and this is the part that
                // surprises.** Recording the shed to the tenant needs `Meters`,
                // which lives behind the module table — a read lock this
                // request has otherwise never touched, because it is refused
                // here and never reaches `run_dispatch`. A **triple**: ~34 ns
                // held, hashing a `ModuleId` and cloning an `Arc`, against a
                // 20 ns mark. Inlined rather than calling `Cell::meters` so
                // the acquisition can be marked from inside.
                marks::mark(&j.marks, Mark::ShedMetersLockWanted);
                let modules = self.modules.read();
                marks::mark(&j.marks, Mark::ShedMetersLockAcquired);
                let meters = modules.get(module).map(|e| e.meters.clone());
                drop(modules);
                marks::mark(&j.marks, Mark::ShedMetersLockReleased);
                if let Some(m) = meters {
                    m.record_shed();
                }
                SubmitOutcome {
                    admitted: false,
                    refused_marks: j.marks.take(),
                }
            }
        }
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
        // Through the runtime rather than inline, because the vtable's own
        // `settle` is the *other* door and is the one every guest-awaited
        // future comes through. They were two copies of the same three lines,
        // and instrumenting only this one measured 1% of a real run's pauses.
        // See `CellRuntime::settle_future`.
        self.rt.settle_future(id, result);
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
        // **Off the job and into a local, for the rest of the request.** From
        // here the trace is written by this thread and only this thread, so it
        // does not need to live behind the job — and keeping it here is what
        // lets `settle` and `respond` take marks without going through
        // `Cell::active`, which would put the shard's own contention inside
        // every span below.
        let mut marks = job.marks.take();
        marks::mark(&mut marks, Mark::DispatchStarted);
        let job = job;
        let did = job.rec.dispatch_id;
        // A **triple**: ~34 ns held, hashing a `ModuleId` and cloning an
        // `Arc`, against a 20 ns mark. Bound to a local first so the read
        // guard is dropped before the closing mark rather than at the end of a
        // `let ... else`.
        marks::mark(&mut marks, Mark::ModulesLockWanted);
        let modules = self.modules.read();
        marks::mark(&mut marks, Mark::ModulesLockAcquired);
        let entry = modules.get(&job.module).cloned();
        drop(modules);
        marks::mark(&mut marks, Mark::ModulesLockReleased);
        let Some(entry) = entry else {
            // Refused **after** queueing and being given a worker, which is
            // what the span before this mark says and what no previous
            // instrument could have said at all.
            marks::mark(&marks, Mark::ModuleMissing);
            let _ = job.responder.send(DispatchResult {
                status: 404,
                body: b"no such module".to_vec(),
                marks: marks.take(),
                ..DispatchResult::shed(did)
            });
            return;
        };

        marks::mark(&mut marks, Mark::InstanceAcquireBegan);
        let acquired;
        let slot = match &job.target {
            Target::Handler { .. } => {
                let t0 = Instant::now();
                match self.acquire_handler(&entry, &mut marks) {
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
                        // Refused after the most expensive part of setup is
                        // already paid for — possibly after a whole activation.
                        marks::mark(&marks, Mark::AcquireFailed);
                        let _ = job.responder.send(DispatchResult {
                            status: 500,
                            body: e.to_string().into_bytes(),
                            marks: marks.take(),
                            ..DispatchResult::shed(did)
                        });
                        return;
                    }
                }
            }
        };

        marks::mark(&mut marks, Mark::InstanceAcquireEnded);

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
        // A **pair**: 16.9 ns held, well under one mark. This and the insert
        // thirty lines below are two separate acquisitions of the same lock on
        // the same key, which is a thing the span names make visible and an
        // `insert_if_absent` on the `ActiveMap` trait would collapse.
        marks::mark(&mut marks, Mark::ActiveProbeWanted);
        let already_in_flight = self.active.contains_key(&slot.id);
        marks::mark(&mut marks, Mark::ActiveProbeReleased);
        if already_in_flight {
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
            marks::mark(&marks, Mark::SingleWriterRefused);
            let _ = job.responder.send(DispatchResult {
                status: 409,
                body: "durable instance already has a dispatch in flight (§7.2)"
                    .as_bytes()
                    .to_vec(),
                marks: marks.take(),
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
        // A **triple**: the hold is 60 ns and more — two map inserts and a set
        // insert, any of which can grow — against a 20 ns mark, so the wait
        // for this global table separates cleanly from the work under it.
        marks::mark(&mut marks, Mark::CancelAllocWanted);
        job.rec.cancel_future =
            self.rt
                .futures
                .alloc_cancellation_marked(slot.id, job.rec.dispatch_id, &mut marks);
        marks::mark(&mut marks, Mark::CancelAllocReleased);
        slot.ctx.set_cancel_future(job.rec.cancel_future);

        marks::mark(&mut marks, Mark::ActiveInsertWanted);
        self.active.insert(
            slot.id,
            ActiveRequest {
                dispatch_id: job.rec.dispatch_id,
                lifecycle: entry.manifest.handlers.lifecycle,
                module: job.module.clone(),
                route: job.target.route(),
                responder: Some(job.responder),
                started_ns: job.enqueued_ns,
                cpu_ns: 0,
                wall_ns: 0,
                slices: 0,
                acquired,
                budget,
                marks: None,
            },
        );
        marks::mark(&mut marks, Mark::ActiveInsertReleased);

        // From here the request is answerable, so from here a panic on this
        // thread can answer it. See `Cell::answer_after_panic`.
        IN_FLIGHT.set(Some((slot.id, job.rec.dispatch_id)));

        entry.meters.record_dispatch();
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);

        slot.ctx
            .begin_dispatch(crate::now_ms(), job.rec.dispatch_id);

        let outcome = {
            // A **triple**, and the first measurement this lock has ever had:
            // `Slot::inner` is a plain `parking_lot::Mutex`, not a
            // `CountedMutex`, so it is in no row of `Cell::lock_stats` — and
            // every dispatch and every resumed slice takes it twice.
            marks::mark(&mut marks, Mark::SlotEnterWanted);
            let mut guard = match slot.enter_marked(&mut marks) {
                Ok(g) => {
                    marks::mark(&mut marks, Mark::SlotEnterReleased);
                    g
                }
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
                    marks::mark(&marks, Mark::EnterRefused);
                    self.respond(
                        &slot,
                        DispatchResult {
                            status: 409,
                            body: b"instance busy".to_vec(),
                            ..DispatchResult::shed(did)
                        },
                        &mut marks,
                    );
                    self.active.remove(&slot.id);
                    return;
                }
            };
            self.hot.fetch_add(1, Ordering::Relaxed);
            marks::mark(&mut marks, Mark::GuestEntered);
            let out = guard.instance().dispatch(&job.rec, budget);
            // **The guest call, host seam included.** Strictly larger than the
            // engine's own `DispatchOutcome::wall_ns`, which is taken inside
            // the engine and misses `enter_hot`, `begin_slice`, the epoch
            // deadline, trap classification and `exit_hot`. Both are published;
            // their difference is the seam, and nothing measured it before.
            marks::mark(&mut marks, Mark::GuestReturned);
            self.hot.fetch_sub(1, Ordering::Relaxed);
            marks::mark(&mut marks, Mark::SlotReleaseWanted);
            // **Tombstone before releasing, or not at all.** See
            // `ends_in_tombstone`: releasing first leaves a window in which
            // another thread can enter an instance a trap has abandoned.
            if ends_in_tombstone(&out, &slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            out
        };
        // A **pair**: the release path is reached through `impl Drop for
        // CallGuard`, which takes no arguments and so cannot be handed a
        // buffer to mark into — see the note in `crate::marks`.
        marks::mark(&mut marks, Mark::SlotReleaseReleased);

        // A completion can land while a dispatch is Hot; §5.4 made it wait, so
        // if one did, hand the instance to the wake lane rather than leaving a
        // result queued against a parked instance nobody will visit again.
        if !self.settle(&slot, &entry, outcome, &mut marks) {
            self.queue_wake(slot);
        }
    }

    /// **Hand the trace to the parked request**, since no thread will hold it
    /// until a worker picks the wake up.
    ///
    /// `SliceEnded` is marked here rather than at the caller, so it is the last
    /// thing on the trace before it goes dormant — the span that opens at it is
    /// the wait for the outside world, and anything marked after it would be
    /// inside that wait rather than before it.
    ///
    /// Costs one `Cell::active` acquisition, on the park path only, and only
    /// for a traced request: an untraced one has nothing to stash and the
    /// `Option` is empty before the lock is reached.
    fn park_marks(&self, slot: &Arc<Slot>, marks: &mut Option<RequestMarks>) {
        if marks.is_none() {
            return;
        }
        marks::mark(marks, Mark::SliceEnded);
        self.stash_marks(slot, marks.take());
    }

    /// **Give a reclaimed trace back to the request it belongs to.**
    ///
    /// `Cell::run_wake` takes the parked request's trace out of `Cell::active`
    /// before it knows whether this wake is going to run a slice, and there are
    /// six ways it can then decide not to: the instance is busy, tombstoned,
    /// gone from `residents`, its module has been deregistered, or the future
    /// table has nothing ready for it. Every one of those used to `return` with
    /// the trace in a local, which dropped it — so the request stayed parked
    /// and **untraced for the rest of its life**, and the reply it eventually
    /// sent carried nothing. A wake that runs nothing is common: several
    /// completions for one instance produce several wakes, and the first one's
    /// loop drains all of them.
    ///
    /// Costs one `Cell::active` acquisition on paths that are already the
    /// expensive ones, and only for a traced request.
    fn stash_marks(&self, slot: &Arc<Slot>, taken: Option<RequestMarks>) {
        if taken.is_none() {
            return;
        }
        // A request whose `active` entry has already gone is one nobody will
        // wake; letting the trace drop returns its buffer rather than leaking
        // it, and the trace simply ends where the request did.
        //
        // **Not counted, because of how narrow it is.** Reaching here with a
        // trace in hand means one was reclaimed, which means the wake was
        // fresh, which means the entry existed a moment ago — so the entry has
        // to be removed *between* the reclaim and this line, and only
        // `finish_and_release` and `answer_after_panic` remove one. Both run on
        // the thread that answered the request, and this request is parked. The
        // way it happens is the race `c7d7730` describes from the other side: a
        // second wake adopts and answers the same parked request while this
        // thread is still deciding not to run a slice. A counter here would be
        // a number nobody could make move on purpose, which is the shape this
        // codebase has had to delete once already.
        if let Some(orphan) = self.active.with_mut(&slot.id, |a| a.marks = taken) {
            let _ = orphan;
        }
    }

    /// **Close a wake that is not going to run a slice**: file whatever the
    /// carrier has collected, then hand the trace back.
    ///
    /// One place rather than six, for the reason `WorkQueue::stamp_taken` is
    /// one place rather than four. `run_wake` reclaims the trace before it can
    /// know whether it will run anything, and there are six ways it can then
    /// decide not to; a path that stashed without filing would lose whatever
    /// the producer wrote after this thread's first pass, and it would lose it
    /// only on the paths nobody thought about.
    ///
    /// **No `fresh` guard, and it does not need one.** Every one of these
    /// returns is above the adoption at `take_ready`, so a stale wake's `marks`
    /// is still `None` here — and filing into a `None` is exactly how a stale
    /// wake's readings are discarded, which is what the unguarded pass at the
    /// top of `run_wake` is for. Filing again costs one pass over ten cleared
    /// cells and cannot resurrect anything: `file` takes each reading as it
    /// files it.
    fn bail_wake(
        &self,
        slot: &Arc<Slot>,
        marks: Option<RequestMarks>,
        stamps: &Option<Arc<crate::marks::WakeStamps>>,
    ) {
        if let Some(st) = stamps {
            st.file(&marks);
        }
        self.stash_marks(slot, marks);
    }

    /// Put an instance in the wake lane, stamped if its request is one of the
    /// sampled ones.
    ///
    /// One place applies the predicate for every producer inside the cell, so
    /// the three push sites cannot come to differ about which requests carry a
    /// queue stamp. `CellWake::signal` is the fourth and cannot use this — it
    /// holds no cell — so it carries the same two lines against the same id.
    fn queue_wake(&self, slot: Arc<Slot>) {
        // `arrived` is *now* on this path, and that is the truthful reading:
        // the completion landed while the dispatch was still Hot, so by the
        // time the request parked there was nothing left to wait for. The
        // request's `park.wait` is then a handful of nanoseconds, which is what
        // it should say.
        //
        // The completion-path fields stay unset. This wake did not come from a
        // reactor and there is no future-table acquisition or index lookup to
        // attribute to it; `WakeStamps::file` skips a reading that was never
        // taken rather than filing a zero, which would be a foreign timestamp
        // and would be refused anyway.
        let stamps =
            stage_sampled(slot.ctx.dispatch_id(), self.cfg.stage_sample_every).then(|| {
                let st = Arc::new(marks::WakeStamps::default());
                st.arrived.take();
                st
            });
        // **The slot's current dispatch, and that is the same rule the
        // completion path uses rather than a second one.** The wake the
        // reactor pushes names the dispatch that *created the future*, because
        // that is whose wait the readings measure. Here there is no future and
        // no wait: this wake exists because the slice that just ran left ready
        // work behind, and the only reading on it was taken on this thread a
        // few instructions ago. Both name the dispatch the readings belong to.
        //
        // It can still go stale, and by the same route: this is pushed while
        // the dispatch is on the slot, and a lane can hold it past the end of
        // that dispatch. Naming it here is what lets `run_wake` notice.
        let dispatch = Some(slot.ctx.dispatch_id());
        // Counted because it is otherwise invisible: this path does not go
        // through `CellWake::signal_traced`, so it bumps neither `wakes` nor
        // anything else, and it is the shape whose traces have no completion
        // prefix. See `Metrics::in_cell_wakes`.
        self.metrics.in_cell_wakes.fetch_add(1, Ordering::Relaxed);
        self.queue.push_wake(WakeItem {
            slot,
            stamps,
            dispatch,
        });
    }

    /// One woken slice, and every further slice the instance can take without
    /// going back through a lane.
    ///
    /// `queued` is when this wake entered its lane, on the requests that are
    /// sampled. It is charged to [`StageTimings::queue_ns`], which is the
    /// per-request sum across slices; the loop below does not re-queue, so the
    /// stamp is spent once and the later slices in this call queued for
    /// nothing and are charged nothing.
    fn run_wake(
        self: &Arc<Self>,
        slot: Arc<Slot>,
        stamps: Option<Arc<crate::marks::WakeStamps>>,
        dispatch: Option<u64>,
    ) {
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

        // **Is this wake still about the request on the slot?**
        //
        // It is routed by instance, and `Cell::active` holds one entry per
        // instance — so the trace reclaimed below is whoever is in flight
        // *now*, not whoever awaited this completion. Those are the same
        // request almost always, and §7.2's single-writer refusal is why:
        // a second dispatch cannot begin while the first is still in `active`.
        // But a lane holds a wake for as long as its worker is busy and
        // `WorkQueue::pop` serves `lanes[worker % n]` with no stealing, so a
        // wake pushed during one dispatch can be popped after that dispatch
        // has finished, the instance has been handed to the next request, and
        // that request has parked on the same slot.
        //
        // Everything after this point is then a stranger's: the ten carried
        // readings measure a wait the new request never had, and the readings
        // taken before its trace began wrap when `read` differences them
        // against its origin. `Trace::mark_at` stops the wrap, which is what
        // makes this survivable; it does not stop the rest.
        //
        // One `u64` compare against a field already in hand, on every wake.
        let current = slot.ctx.dispatch_id();
        let fresh = dispatch.is_none_or(|d| d == current);
        if !fresh {
            // §10, and named beside the other instrument faults: a run that
            // loses a request's spans this way says how many rather than
            // publishing somebody else's.
            self.metrics.stale_wakes.fetch_add(1, Ordering::Relaxed);
        }
        // **Reclaim the trace the park left behind.**
        //
        // Guarded on the sampling predicate — one atomic load and a modulo,
        // no lock — so an untraced wake does not take `Cell::active` here at
        // all. A traced one does, and pays for it: this is a fifth lock on the
        // wake path and the span it opens is charged with taking it.
        //
        // **A stale wake reclaims nothing, and so marks nothing.** Every
        // `marks::mark` below is a no-op on a `None`, which is what makes
        // refusing the trace enough on its own: the wake still runs, the
        // instance still gets its result, and the request on the slot keeps a
        // trace with no foreign spans in it. Refused rather than re-labelled,
        // for the reason `Trace::mark_at` refuses: the labels are positional —
        // a span takes the name of the mark that *ends* it — so a trace joined
        // half way through does not merely gain spans, it renames the ones it
        // already had.
        let mut marks: Option<RequestMarks> = (fresh
            && stage_sampled(current, self.cfg.stage_sample_every))
        .then(|| self.active.with_mut(&slot.id, |a| a.marks.take()).flatten())
        .flatten();
        // **Everything the completion path measured, filed at last.** Ten
        // readings taken on three threads that could none of them reach this
        // trace: the reactor that completed the future, whoever pushed the
        // wake, and whichever consumer popped it. See `marks::WakeStamps`.
        //
        // Filed *after* the reclaim above, because the reclaim is what makes
        // the trace reachable at all; the readings carry their own timestamps
        // and `trace::read` sorts, so where they are filed does not move them.
        // Filed here **and again below**, just before the request is handed to
        // `settle`. `file` clears each reading as it takes it, so nothing lands
        // twice; the second pass exists because the producer is still writing
        // when this one runs. `notified` in particular is written after a
        // `notify_one` that is a futex syscall — microseconds, longer than this
        // thread takes to get here — and the readings that would be lost are
        // exactly the slow ones. See `marks::WakeStamps`.
        //
        // **Deliberately not guarded on `fresh`, unlike its twin below.** For a
        // stale wake `marks` is `None` here, and filing into a `None` is how
        // these readings are *discarded* — `file` takes each one and drops it.
        // That is the refusal doing its job, and the pass below depends on it:
        // guard this and every reading survives to be filed into the trace the
        // adoption picks up.
        if let Some(st) = &stamps {
            st.file(&marks);
        }
        marks::mark(&marks, Mark::WakeStarted);
        // A wake runs an existing request, which is therefore already in
        // `Cell::active` and already answerable. The id comes from the
        // instance's own context, which `begin_dispatch` set and every slice
        // of this request carries. See `Cell::answer_after_panic`.
        IN_FLIGHT.set(Some((slot.id, slot.ctx.dispatch_id())));
        // A **triple**: ~48 ns held, with a `ModuleId::clone` inside the hold.
        // Deliberately its own label rather than sharing the dispatch path's:
        // the two are the same lock under different contention, and one row
        // covering both could not say which side was waiting.
        marks::mark(&marks, Mark::WakeResidentsLockWanted);
        let residents = self.residents.lock();
        marks::mark(&marks, Mark::WakeResidentsLockAcquired);
        let module = residents.get(&slot.id).map(|r| r.module.clone());
        drop(residents);
        marks::mark(&marks, Mark::WakeResidentsLockReleased);
        // **Every early return below goes through `bail_wake`**, which files
        // the carrier and hands the trace back. The trace was taken out of
        // `Cell::active` above, before this function could know whether it was
        // going to run a slice at all; dropping it here would leave the request
        // parked and untraced for the rest of its life, and the reply it
        // eventually sends would carry nothing. See `Cell::bail_wake`.
        let Some(module) = module else {
            self.bail_wake(&slot, marks, &stamps);
            return;
        };
        let Some(entry) = self.modules.read().get(&module).cloned() else {
            self.bail_wake(&slot, marks, &stamps);
            return;
        };

        loop {
            // Take exclusive ownership of the instance *before* taking the
            // result out of the future table. The other order has a hole: a
            // result pulled out with nowhere to deliver it has been silently
            // dropped, and a dropped wake is a request that never finishes.
            marks::mark(&marks, Mark::SlotEnterWanted);
            let mut guard = match slot.enter_marked(&mut marks) {
                Ok(g) => {
                    marks::mark(&marks, Mark::SlotEnterReleased);
                    g
                }
                Err(crate::instances::EnterError::Busy) => {
                    // §5.4: a previous call into this instance has not
                    // returned. This is the reentrancy rule doing its job —
                    // the result stays queued and the in-flight call drains it
                    // when it finishes.
                    self.metrics
                        .reentrancy_refusals
                        .fetch_add(1, Ordering::Relaxed);
                    self.bail_wake(&slot, marks, &stamps);
                    return;
                }
                Err(e @ crate::instances::EnterError::Tombstoned) => {
                    // A wake queued for an instance that was tombstoned before
                    // the wake lane reached it. §6.4 refuses, and §10 counts.
                    self.count_enter_refusal(e);
                    self.bail_wake(&slot, marks, &stamps);
                    return;
                }
                Err(_) => {
                    self.bail_wake(&slot, marks, &stamps);
                    return;
                }
            };
            // A **triple**: 80 ns and more — a loop popping and removing across
            // three structures — so the wait for the table and the work under
            // it separate cleanly.
            marks::mark(&marks, Mark::TakeReadyWanted);
            let ready = self.rt.futures.take_ready_marked(slot.id, &mut marks);
            marks::mark(&marks, Mark::TakeReadyReleased);
            let Some((fid, res)) = ready else {
                // **The common stale wake.** Several completions for one
                // instance produce several wakes, and the first one's loop
                // drains all of them; the rest arrive to an empty table. The
                // instance is still parked and its request still needs its
                // trace.
                drop(guard);
                self.bail_wake(&slot, marks, &stamps);
                return;
            };
            // **A stale wake that is going to run a slice picks the trace up
            // here, having refused it at the top.**
            //
            // The refusal above is about the wake's *own* readings and its own
            // path, which belong to a dispatch that has gone home. This is a
            // different claim: the instance has ready work, and whatever is on
            // the slot now is what a slice will advance — its guest time, its
            // CPU, its response. That is the current request's cost however
            // stale the doorbell that produced it, and leaving it unmeasured
            // does not make the request cheaper, it makes it invisible. Before
            // this, a stale wake that answered a request took that request's
            // whole trace down with it: `respond` builds the reply from the
            // local hand, so an empty hand ships an empty reply while the real
            // trace sits in `Cell::active` waiting to be dropped by
            // `finish_and_release`.
            //
            // `Mark::WakeAdopted` is taken with it and is not optional: labels
            // are positional, so a trace rejoined without one has its pause
            // absorbed into the next mark's name.
            //
            // Costs one `Cell::active` acquisition, on the stale-and-running
            // path only — which `Metrics::stale_wakes` says is rare, and which
            // was already the expensive one.
            if !fresh && marks.is_none() && stage_sampled(current, self.cfg.stage_sample_every) {
                marks = self.active.with_mut(&slot.id, |a| a.marks.take()).flatten();
                marks::mark(&marks, Mark::WakeAdopted);
            }
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
            // A **pair**: ~20 ns held.
            marks::mark(&marks, Mark::ActiveBudgetWanted);
            let budget = self
                .active
                .with_mut(&slot.id, |a| a.budget.after(a.cpu_ns))
                .unwrap_or_else(CpuBudget::http);
            marks::mark(&marks, Mark::ActiveBudgetReleased);

            // §8.5: a wake is a new slice, so the coarse clock advances here
            // and nowhere else.
            slot.ctx
                .begin_slice(crate::now_ms(), slot.ctx.dispatch_id());

            let t0 = Instant::now();
            self.hot.fetch_add(1, Ordering::Relaxed);
            marks::mark(&marks, Mark::GuestEntered);
            let outcome = guard.instance().wake(fid, &res, budget);
            marks::mark(&marks, Mark::GuestReturned);
            self.hot.fetch_sub(1, Ordering::Relaxed);
            marks::mark(&marks, Mark::SlotReleaseWanted);
            // Same ordering as the dispatch path, and for the same reason: a
            // resumed slice that traps must not leave the sandbox enterable
            // while `settle` is still on its way to the tombstone.
            if ends_in_tombstone(&outcome, &slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            marks::mark(&marks, Mark::SlotReleaseReleased);
            self.metrics.wake_us.record(t0.elapsed().as_micros() as u64);

            // **The second pass over the carried readings**, and the last
            // point at which the trace is still on this thread: `settle` can
            // reach `respond`, which moves the buffer into the reply. By now
            // the guest's own slice has run, so anything the producer wrote
            // after the push has certainly landed.
            //
            // **`fresh` guards a race, and only a race.** At the top of this
            // function a stale wake holds no trace, and filing into a `None`
            // is how its readings are discarded — taken and dropped. By this
            // line it may hold one, adopted at the `take_ready` above, so the
            // question is whether anything is left to file into it. Almost
            // always nothing is: the pass at the top emptied every cell.
            //
            // What can still be here is exactly what this second pass exists
            // for — a reading the *producer* wrote after that first pass, on
            // the other side of the handoff, which is why it keeps a handle
            // across the push at all. Those are lane readings from a dispatch
            // that has already answered, and the adopted trace must not have
            // them.
            //
            // So this is a guard against an interleaving no test here can
            // force, kept because the alternative is a foreign
            // `wake.lane.lock.hold` appearing on one request in a thousand
            // with nothing to explain it. Removing it does not fail the suite;
            // that is a statement about what is reproducible, not about
            // whether it can happen.
            if fresh {
                if let Some(st) = &stamps {
                    st.file(&marks);
                }
            }
            // **The loop may run the guest several times per wake**, without
            // going back through a lane. Every pass marks its own group, which
            // is the half the old instrument could not see at all: `waited` is
            // taken once, correctly, but the second and later slices' entries,
            // reads and settles were invisible.
            if self.settle(&slot, &entry, outcome, &mut marks) {
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
        marks: &mut Option<RequestMarks>,
    ) -> bool {
        // §11.2: record the Hot interval. Nothing else is ever recorded — and on
        // a kill of either phase that interval *includes the overage*, because
        // the meter reads the thread clock and does not know what a budget is
        // (§6.4: "the overage CPU is recorded").
        marks::mark(marks, Mark::TenantRecordWanted);
        entry.meters.record_cpu(&out);
        if out.grace_deferrals > 0 {
            self.metrics
                .grace_deferrals
                .fetch_add(out.grace_deferrals, Ordering::Relaxed);
        }
        // A **triple**: 26.6 ns held at eight tenants, and it allocates a
        // `String` from the tenant name inside the hold before walking a
        // `BTreeMap` — so the hold grows with the tenant count while the wait
        // grows with the thread count, and they are worth telling apart.
        // `Metrics::by_tenant` is not a `CountedMutex` and appears in no row
        // of `Cell::lock_stats`, so nothing else measures it at all.
        self.metrics.record_tenant_marked(
            &entry.tenant,
            out.cpu_ns,
            !matches!(out.completion, Completion::Done | Completion::Parked),
            marks,
        );
        marks::mark(marks, Mark::TenantRecordReleased);

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

        // A **triple**, and the only one `Cell::active` earns: this hold is
        // ~65 ns because the module and the route are *cloned inside it*,
        // against ~20 ns at every other site. If `Cell::active` is the cell's
        // largest source of blocked time, this is the one critical section a
        // shortening would actually pay for.
        marks::mark(marks, Mark::ActiveUpdateWanted);
        let (lifecycle, module, route) = self
            .active
            .with_mut(&slot.id, |req| {
                marks::mark(marks, Mark::ActiveUpdateAcquired);
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
        marks::mark(marks, Mark::ActiveUpdateReleased);
        let breaker_key = BreakerKey { module, route };

        match completion {
            Completion::Parked => {
                slot.park(crate::now_ms());
                marks::mark(marks, Mark::Parked);
                // A completion may have landed while we were Hot; §5.4 made it
                // wait, so drain it now. A **pair**: one map lookup and an
                // emptiness check.
                marks::mark(marks, Mark::ReadyCheckWanted);
                let ready = self.rt.futures.has_ready(slot.id);
                marks::mark(marks, Mark::ReadyCheckReleased);
                self.park_marks(slot, marks);
                !ready
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
                // A **triple**, and conditional: only a dispatch that
                // completed with a response handle reaches it. The hold can
                // clone the whole response body, so its cost is the size of
                // what the guest produced rather than a constant — which is
                // exactly the shape worth separating from the wait.
                let response = match out.status {
                    PollStatus::Done { response } if response != 0 => {
                        marks::mark(marks, Mark::ResponseTableWanted);
                        let mut t = slot.ctx.responses.lock();
                        marks::mark(marks, Mark::ResponseTableAcquired);
                        let r = match t.get(response) {
                            Some(r) if r.head.streaming && !r.finished => Some(r.clone()),
                            _ => t.take(response),
                        };
                        drop(t);
                        marks::mark(marks, Mark::ResponseTableReleased);
                        r
                    }
                    _ => None,
                };
                let streaming_open = response
                    .as_ref()
                    .is_some_and(|r| r.head.streaming && !r.finished);
                // A **triple**: the hold walks the `wait_until` set with a
                // map lookup per element, so it is a function of what the guest
                // did rather than a constant.
                marks::mark(marks, Mark::WaitUntilCheckWanted);
                let outstanding = self
                    .rt
                    .futures
                    .wait_until_outstanding_marked(slot.id, marks);
                marks::mark(marks, Mark::WaitUntilCheckReleased);
                let wait_until = outstanding || streaming_open;

                let (status, headers, body) = match &response {
                    Some(r) => (r.head.status, r.head.headers.clone(), r.body()),
                    None => (204, vec![], Vec::new()),
                };
                let result =
                    self.build_result(slot, status, headers, body, completion, None, marks);
                self.respond(slot, result, marks);

                match decide(lifecycle, completion, wait_until) {
                    RequestEnd::RetainedForWaitUntil => {
                        // §2.2/§2.6: the instance lives past the response and,
                        // under `fresh`, the reset is still owed.
                        slot.park(crate::now_ms());
                        marks::mark(marks, Mark::Parked);
                        marks::mark(marks, Mark::ReadyCheckWanted);
                        let ready = self.rt.futures.has_ready(slot.id);
                        marks::mark(marks, Mark::ReadyCheckReleased);
                        self.park_marks(slot, marks);
                        !ready
                    }
                    RequestEnd::Disposed(d) => {
                        // **Not marked, and it cannot be**: `respond` above has
                        // already sent the reply, and the trace travels inside
                        // it. Everything from here is the cell's tail — real
                        // work that holds the worker, but not part of the
                        // request's elapsed time, because the caller already
                        // has its answer. See `crate::marks` on where the
                        // trace ends.
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
                    marks,
                );
                self.respond(slot, result, marks);
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
                    // §6.4: only HARD kills trip the hang breaker. A
                    // **triple**, and the second acquisition of this lock in
                    // one request — the first was the admission check, a whole
                    // dispatch ago. The hold clones a `BreakerKey` into
                    // `HashMap::entry` before hashing it, which is three
                    // `String` allocations under the lock.
                    marks::mark(marks, Mark::BreakerKillWanted);
                    self.breaker
                        .record_kill_marked(&breaker_key, crate::now_ms(), marks);
                    marks::mark(marks, Mark::BreakerKillReleased);
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
                // A **triple**: the hold is a linear scan of up to 256 ring
                // entries before the push, so it is a few hundred nanoseconds
                // at a full ring rather than a constant — and every trap and
                // every hard kill takes it, which is exactly the traffic a
                // cell under stress is producing.
                marks::mark(marks, Mark::TombstoneRingWanted);
                self.remember_tombstone_marked(slot, marks);
                marks::mark(marks, Mark::TombstoneRingReleased);
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
                    marks,
                );
                self.respond(slot, result, marks);
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
        marks: &mut Option<RequestMarks>,
    ) -> DispatchResult {
        // A **pair**: the hold is ~20 ns, half what a mark costs. Note this is
        // the *third* separate acquisition of `Cell::active` on the same key
        // in one request — the probe, the update in `settle`, and this — which
        // the span names make visible for the first time.
        marks::mark(marks, Mark::ResultReadWanted);
        let (dispatch_id, cpu_ns, wall_ns, slices, acquired) = self
            .active
            .with(&slot.id, |r| {
                (r.dispatch_id, r.cpu_ns, r.wall_ns, r.slices, r.acquired)
            })
            .unwrap_or((0, 0, 0, 0, "unknown"));
        marks::mark(marks, Mark::ResultReadReleased);
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
            // Filled by `Cell::respond`, which is the last thing to hold the
            // buffer before it crosses the channel. A result built and never
            // sent therefore carries no trace, which is the truthful state:
            // nobody was answered.
            marks: None,
        }
    }

    fn respond(&self, slot: &Arc<Slot>, result: DispatchResult, marks: &mut Option<RequestMarks>) {
        // A **pair**: ~18 ns held, and the fourth acquisition of `active`.
        marks::mark(marks, Mark::ResponderTakenWanted);
        let tx = self
            .active
            .with_mut(&slot.id, |a| a.responder.take())
            .flatten();
        marks::mark(marks, Mark::ResponderTakenReleased);
        if let Some(tx) = tx {
            // Stamped after the `active` shard is released and immediately
            // before the send, so the interval the caller reconstructs is the
            // channel and nothing else. Inside the lock it would carry the
            // shard's own contention, which `lock_stats` already reports under
            // its own name — one cost, counted twice, in two tables that are
            // read together.
            let mut result = result;
            // The trace boards the result here, and `ReplyHandedOff` is taken
            // once it has — after the `active` shard is released and
            // immediately before the send, so the shard's own contention (which
            // `lock_stats` reports under its own name) is not counted a second
            // time inside the reply figure.
            //
            result.marks = marks.take();
            marks::mark(&result.marks, Mark::ReplyHandedOff);
            // **Non-blocking first, and the fallback is the whole measurement.**
            //
            // `SyncSender::send` blocks when the channel is full, the sender is
            // an execution-pool thread, and that block is latency the request
            // really pays — so it is worth a span. But a mark taken after an
            // unconditional `send` measures the wrong thing and races the
            // reader for it.
            //
            // Wrong thing: on a channel with room, `send` does not block at
            // all, and what the mark then times is this thread's own wake-up of
            // a parked receiver — the *answerer's* cost, exactly the quantity
            // `Mark::WorkerWoken` was moved off the producer to avoid.
            //
            // Races: the trace travels *inside* the result, and the collector's
            // very next actions after `recv` are its own last mark and
            // `marks::read`. There is no guest call between the handoff and the
            // read the way there is on the queue handoffs, so a reading taken
            // here can land after the trace has already been reduced — and the
            // ones it loses are the slow sends, which is the worst possible
            // bias for a row about blocking.
            //
            // `try_send` first removes both. When it succeeds nothing is
            // marked and there was nothing to measure — which is the common
            // case and the whole of the fix for the second problem, since a
            // reading that is never taken cannot be lost. When it comes back
            // `Full` the channel really is full, which is the case the row
            // exists for.
            //
            // **The residual, stated rather than glossed.** On a channel
            // deeper than one the race is gone by construction: a blocked send
            // returns when the receiver takes the *head*, and this result is at
            // the tail behind everything already queued, so the collector has a
            // whole channel to drain before it can reach this trace. At depth
            // one there is no such gap — the item taken is the only item, and
            // ours becomes the head immediately. No traced path has one:
            // `wisp-bench` gives the collector `sync_channel(1 << 16)` and
            // `Cell::submit_with`'s depth-one channel passes `marks: None`. The
            // tests that do pair a depth-one channel with a trace wait for the
            // worker to finish rather than assuming the ordering.
            match tx.try_send(result) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(result)) => {
                    let tail = marks::writer(&result.marks);
                    let _ = tx.send(result);
                    marks::mark_after(&tail, Mark::SendReturned);
                }
                // Nobody is listening. Same outcome as the `let _ = send(..)`
                // this replaces: the result is dropped.
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
            }
        }
    }

    fn finish_and_release(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        d: crate::instances::Disposition,
    ) {
        // **The tail's own trace opens here.** The request's is gone — it
        // travelled inside the reply `Cell::respond` has already sent — and
        // everything below this line is work that holds an execution thread
        // without being any part of the request's elapsed time. See
        // `crate::marks` on where the trace ends, and on why these spans are
        // reported somewhere a reader cannot add them to a request's.
        let tail = self.tail_begin(slot);

        marks::mark(&tail, Mark::FinishBegan);

        // A **pair**, and the fifth acquisition of `Cell::active` on this key
        // in one request: the hold is a hash and a `remove` that moves the
        // entry out, cloning nothing, so it is at about what one mark costs.
        marks::mark(&tail, Mark::ActiveRemoveWanted);
        let req = self.active.remove(&slot.id);
        marks::mark(&tail, Mark::ActiveRemoveReleased);
        if let Some(r) = &req {
            // A **triple**: the hold clones a `BTreeSet`, allocates a `Vec` of
            // every pending id and walks three structures.
            marks::mark(&tail, Mark::EndDispatchWanted);
            self.rt
                .futures
                .end_dispatch_marked(slot.id, r.dispatch_id, &tail);
            marks::mark(&tail, Mark::EndDispatchReleased);
            self.metrics
                .request_us
                .record(crate::mono_ns().saturating_sub(r.started_ns) / 1_000);
            self.metrics.park_ms.record(slot.parked_ms_total());
            entry.meters.record_park(slot.parked_ms_total());
        }

        if !d.is_warm() && !matches!(d.kind(), crate::instances::DispositionKind::Reset) {
            self.forget(slot);
        }
        // A **triple**, and the one worth reading. The hold is
        // `entries.retain(|_, e| e.owner != owner)` — a walk of **every entry
        // in the whole cell's future table**, not just this instance's — so
        // what it costs is a function of what every *other* instance is doing,
        // and every dispatch that ends pays it.
        marks::mark(&tail, Mark::ReleaseInstanceWanted);
        self.rt.futures.release_instance_marked(slot.id, &tail);
        marks::mark(&tail, Mark::ReleaseInstanceReleased);
        // A **pair**, and not a lock pair: what happens in here belongs to the
        // disposition. `Warm` is a park and a push; `Reset` runs a whole
        // `Slot::reset` before it reaches the mutex; `Discard` gives the
        // memory back. One row over all three on purpose — the question is how
        // long the worker spends not taking the next request.
        marks::mark(&tail, Mark::PoolReleaseWanted);
        entry.pool.release(slot.clone(), d);
        marks::mark(&tail, Mark::PoolReleaseReleased);
        if slot.state() == LifecycleTier::Discarded {
            self.forget(slot);
        }
        marks::mark(&tail, Mark::FinishEnded);
        Self::tail_stash(tail);
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
        self.remember_tombstone_marked(slot, &mut None)
    }

    /// [`Cell::remember_tombstone`], marking the wait for the ring separately
    /// from the scan done under it.
    fn remember_tombstone_marked(&self, slot: &Arc<Slot>, marks: &mut Option<RequestMarks>) {
        /// Enough to cover the burst a breaker takes to notice, and small
        /// enough that the ring is a rounding error next to one instance.
        const RING: usize = 256;
        let mut g = self.tombstoned.lock();
        marks::mark(marks, Mark::TombstoneRingAcquired);
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
        marks: &mut Option<RequestMarks>,
    ) -> crate::Result<(Arc<Slot>, AcquireSource)> {
        let (slot, src) = entry
            .pool
            .acquire_marked(|id| self.make_ctx(entry, id), marks)?;
        // A **pair**: one `Weak` downgrade and one map insert, about 20 ns,
        // which is at or under what a mark costs. It is here rather than
        // folded into `acquire.rest` because it is a per-cell singleton mutex
        // taken on *every* handler acquisition, warm ones included, and it
        // appears in no row of `Cell::lock_stats`.
        marks::mark(marks, Mark::IndexLockWanted);
        self.index.lock().insert(slot.id, Arc::downgrade(&slot));
        marks::mark(marks, Mark::IndexLockReleased);
        // A **triple**: ~48 ns held, because a `ModuleId` — two `String`
        // allocations — is cloned inside the hold.
        marks::mark(marks, Mark::ResidentsLockWanted);
        let mut residents = self.residents.lock();
        marks::mark(marks, Mark::ResidentsLockAcquired);
        residents.entry(slot.id).or_insert_with(|| Resident {
            slot: slot.clone(),
            module: entry.activator.module().clone(),
        });
        drop(residents);
        marks::mark(marks, Mark::ResidentsLockReleased);
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
            enqueued_ns: crate::mono_ns(),
            marks: None,
            pushed: marks::NO_STAMP,
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
            stamps: None,
            dispatch: None,
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

    // -----------------------------------------------------------------------
    // the trace, across every queue configuration
    // -----------------------------------------------------------------------

    /// **Every queue configuration produces a trace that tiles the request,
    /// and no two of them claim the same spans.**
    ///
    /// This replaces the old decomposition test, which asked whether four
    /// numbers summed to a bracket. The stronger question now is whether the
    /// spans *cover* the request and whether the set of them is honest about
    /// which structure produced it: a lock-free queue has no lane mutex to
    /// wait on, and a row of dashes under `lane.lock.wait` would read exactly
    /// like a lock that was watched and never blocked.
    ///
    /// All nine combinations run, including the four where placement is inert,
    /// because "setting a placement under `Shared` does nothing" is itself
    /// part of what is under test — doing nothing *quietly* would look the
    /// same from outside.
    #[test]
    fn every_queue_configuration_traces_a_request_that_tiles() {
        use crate::marks::Mark;

        for (structure, placement) in every_queue_configuration() {
            let mut cfg = CellConfig::for_test("marks-cfg");
            cfg.queue_structure = structure;
            cfg.queue_placement = placement;
            let cell = Cell::new(cfg, services());
            let m = app(&cell, "echo", FakeBehaviour::default());
            let pool = crate::marks::pool_for(
                crate::tsc_marks::Calibration::measure_for(Duration::from_millis(50)),
                8,
                16,
                4,
            );

            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let t = pool.checkout();
            crate::marks::begin(&t);
            let out =
                cell.submit_with_marks(&m, Target::Handler { route: "/x".into() }, http(0), tx, t);
            assert!(
                out.admitted,
                "{structure:?}/{placement:?} shed a single request"
            );
            let res = rx.recv_timeout(Duration::from_secs(20)).expect("a reply");
            crate::marks::mark(&res.marks, Mark::CallerReceived);
            let trace = crate::marks::read(res.marks.as_ref().expect("a trace"));
            let has = |l: Mark| trace.spans.iter().any(|(k, _)| *k == l);

            // Covered: the request has a beginning, a guest, and an end, and
            // nothing in between ran backwards.
            for l in [Mark::CellEntered, Mark::GuestReturned, Mark::CallerReceived] {
                assert!(has(l), "{structure:?}/{placement:?} lost `{}`", l.name());
            }
            for (l, ns) in &trace.spans {
                assert!(
                    *ns >= 0.0,
                    "{structure:?}/{placement:?}: `{}` is {ns} ns",
                    l.name()
                );
            }
            assert!(
                trace.span_ns > 0.0,
                "{structure:?}/{placement:?} reports no elapsed time"
            );
            assert!(!trace.truncated, "one echo request cannot fill the buffer");

            // …and the set of spans names the structure that produced it.
            let mutexed = structure != QueueStructure::LockFree;
            assert_eq!(
                has(Mark::LaneLockAcquired),
                mutexed,
                "{structure:?}/{placement:?} and `lane.lock.wait` disagree about whether \
                 there is a lane mutex"
            );
            assert_eq!(
                has(Mark::PushRegistered),
                !mutexed,
                "{structure:?}/{placement:?} and `ring.pushing.enter` disagree about whether \
                 there is a ring"
            );
            // The span before the enqueue exists under all of them, which is
            // what makes a table comparing structures compare one quantity.
            assert!(has(Mark::PlacementDecided), "{structure:?}/{placement:?}");
            cell.shutdown();
        }
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
