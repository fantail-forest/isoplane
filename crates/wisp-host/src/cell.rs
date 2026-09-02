//! The cell: reactor threads, the execution pool, and the instance lifecycle.
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
//! *Reactor threads own all I/O.* A completion arrives, the future
//! table maps its id to the owning instance, and a wake is enqueued.
//! *Execution-pool threads run Hot dispatches as **synchronous** calls into
//! the engine.* This is deliberately not the engine's async mode, whose
//! epoch-*yielding* is implemented with fibers — hidden native stacks, which a
//! parked instance could not be captured out of. A native stack exists in this
//! crate only for the duration of one `dispatch`/`wake` call, and a Parked
//! instance holds no thread at all.
//!
//! ## There is no scheduler here, on purpose
//!
//! Fairness is the kernel's job: concurrently Hot dispatches are preempted and
//! time-shared by the OS scheduler, so a CPU-bound tenant occupies only its own
//! thread and cannot head-of-line-block other tenants' wakes. There is no
//! deterministic slice promise here — fairness is statistical and kernel-grade.
//!
//! So this module does **not** time-slice guests, does not run a run-queue
//! with priorities, and does not preempt anybody. What it does control is
//! *admission*: a bounded queue and a shed when it is full. Concurrency is
//! bounded by the size of the execution pool. A spinner burns its own thread
//! until the CPU budget kills it, and the blast radius — the offender's own
//! requests — is a consequence of that arrangement rather than a feature
//! bolted on top.
//!
//! **Not built here:** per-tenant CPU weighting via cgroup v2 threaded `cpu`
//! controllers, and core-scheduling cookies that would keep trust tiers from
//! SMT-pairing. Both need a real cgroup hierarchy and a process per cell, and
//! this runs one process with many cells in it. Fairness is therefore
//! Equal-weight kernel scheduling across execution threads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, RwLock};

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
use crate::sharded_map::ShardedMap;
use crate::{InstanceId, ModuleId};

#[derive(Debug, Clone)]
pub struct CellConfig {
    pub name: String,
    /// Execution-pool threads. Defaults to 4x the cell's logical cpus.
    pub exec_threads: usize,
    /// Reactor worker threads. These own all I/O.
    pub reactor_threads: usize,
    /// Dispatches one worker's lane holds before the cell sheds. Each
    /// execution thread has its own lane, so the cell as a whole holds
    /// `exec_threads` times this.
    pub queue_depth_per_worker: usize,
    pub max_idle_per_module: usize,
    /// Wall-clock backstop for a dispatch; reported separately from active execution time.
    pub wall_ms_cap: u32,
    pub breaker: BreakerConfig,
    /// How often the idle trajectory sweeps.
    pub sweep_interval_ms: u64,
}

impl Default for CellConfig {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        Self {
            name: "cell-0".into(),
            exec_threads: cores * 4,
            reactor_threads: cores.max(2),
            queue_depth_per_worker: 64,
            max_idle_per_module: 64,
            wall_ms_cap: 30_000,
            breaker: BreakerConfig::default(),
            sweep_interval_ms: 250,
        }
    }
}

impl CellConfig {
    /// A small deterministic cell for tests.
    pub fn for_test(name: &str) -> Self {
        Self {
            name: name.into(),
            exec_threads: 4,
            reactor_threads: 2,
            sweep_interval_ms: 25,
            ..Default::default()
        }
    }
}

/// What a top-level work item is addressed to.
#[derive(Debug, Clone)]
pub enum Target {
    /// Any instance of the module — the Handler path.
    Handler { route: String },
}

impl Target {
    fn route(&self) -> String {
        match self {
            Target::Handler { route } => route.clone(),
        }
    }
}

#[derive(Debug)]
pub struct DispatchResult {
    /// The `dispatch_id` of the `DispatchRecord` this is the answer to.
    ///
    /// Echoed back because a caller that submits many dispatches onto **one**
    /// completion channel — which is what any load generator must do, since a
    /// channel and a thread per outstanding request measures the caller — has
    /// no other way to match a result to the request it belongs to.
    /// Completions arrive out of order by design — a wake may land on any pool
    /// thread — so arrival order carries no information.
    pub dispatch_id: u64,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub completion: Completion,
    /// The recorded meter, and nothing else.
    pub cpu_ns: u64,
    /// Observability only; reported separately from active execution time.
    pub wall_ns: u64,
    pub trap: Option<String>,
    pub instance: InstanceId,
    /// Did this request pay for an activation?
    pub acquired: &'static str,
    /// Slices this request took: 1 for a dispatch that never parked, more if
    /// it awaited.
    pub slices: u32,
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
        }
    }
    fn breaker_open(dispatch_id: u64) -> Self {
        Self {
            dispatch_id,
            status: 503,
            headers: vec![],
            // Fail fast, without entering the guest at all.
            body: b"hang breaker open".to_vec(),
            completion: Completion::Trapped,
            cpu_ns: 0,
            wall_ns: 0,
            trap: Some("breaker_open".into()),
            instance: InstanceId(0),
            acquired: "none",
            slices: 0,
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
/// `Discarded` back to `Parked` permanently. It surfaced as an intermittent
/// test failure and was taken for flakiness. The window was never as narrow as
/// `instances.rs` once claimed: that claim held only of the hibernation path,
/// which is the one caller that already tombstoned first.
///
/// The latched case is included and is not decoration: `settle` promotes a
/// latched trap into `Completion::Trapped` a few lines below, so a `Done` whose
/// vtable owed the guest a trap tombstones exactly like an outright one — and
/// asking `take_trap` here instead would consume the latch that promotion needs.
fn ends_in_tombstone(out: &DispatchOutcome, slot: &Arc<Slot>) -> bool {
    matches!(out.completion, Completion::Trapped | Completion::BudgetKill)
        || (matches!(out.completion, Completion::Done | Completion::Parked) && slot.ctx.has_trap())
}

enum Job {
    Dispatch(Box<DispatchJob>),
    /// A host future completed for this instance; deliver it.
    Wake(WakeItem),
}

/// A queued wake.
struct WakeItem {
    slot: Arc<Slot>,
}

struct DispatchJob {
    module: ModuleId,
    target: Target,
    rec: DispatchRecord,
    responder: std::sync::mpsc::SyncSender<DispatchResult>,
    enqueued_ns: u64,
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
/// certain: the pool defaults to 4x the cell's logical cpus, so with fewer
/// shards than threads two threads are guaranteed to share one.
const ACTIVE_SHARDS: usize = 256;

/// Two lanes per worker. Wakes jump ahead of new dispatches, and only a
/// dispatch is ever shed, so work already in flight keeps making progress
/// while a saturated cell refuses new work.
struct WorkQueue {
    lanes: Box<[Lane]>,
    /// The dispatch depth of one lane.
    cap: usize,
}

/// `load` sits on its own cache line so that sampling two lanes' loads never
/// pulls in a line a third lane's push is writing.
#[repr(align(128))]
struct Lane {
    inner: Mutex<QueueInner>,
    cv: Condvar,
    /// Everything queued in this lane, of either kind, plus the job its
    /// worker has taken and not yet finished. Read without the lock by the
    /// placer, so a stale read costs a worse choice rather than a wrong one.
    load: AtomicUsize,
}

#[derive(Default)]
struct QueueInner {
    wakes: std::collections::VecDeque<WakeItem>,
    dispatches: std::collections::VecDeque<Box<DispatchJob>>,
    closed: bool,
}

/// A per-thread xorshift. It runs on every dispatch, so it needs to be cheap
/// and unbiased rather than good.
fn lane_rand(n: usize) -> usize {
    use std::cell::Cell as StdCell;
    thread_local! {
        static STATE: StdCell<u64> = const { StdCell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            // The cell's address differs per thread, so no two workers walk
            // the same sequence.
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
    fn new(depth_per_lane: usize, workers: usize) -> Self {
        Self {
            lanes: (0..workers.max(1))
                .map(|_| Lane {
                    inner: Mutex::new(QueueInner::default()),
                    cv: Condvar::new(),
                    load: AtomicUsize::new(0),
                })
                .collect(),
            cap: depth_per_lane.max(1),
        }
    }

    /// Power of two choices: the less loaded of two lanes drawn at random.
    #[inline]
    fn pick(&self) -> usize {
        let n = self.lanes.len();
        if n <= 1 {
            return 0;
        }
        let a = lane_rand(n);
        let b = lane_rand(n);
        if self.lanes[a].load.load(Ordering::Relaxed) <= self.lanes[b].load.load(Ordering::Relaxed)
        {
            a
        } else {
            b
        }
    }

    /// Releases the load a push added, once the job has run rather than when
    /// it was taken, so a worker inside a long dispatch still reads as busy.
    #[inline]
    fn job_done(&self, worker: usize) {
        let lane = &self.lanes[worker % self.lanes.len()];
        // Saturating, so a decrement without its push cannot wrap the lane
        // into looking infinitely loaded.
        let _ = lane
            .load
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    /// `Err` hands the job back, which is the shed.
    fn push_dispatch(&self, j: Box<DispatchJob>) -> Result<(), Box<DispatchJob>> {
        let lane = &self.lanes[self.pick()];
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

    /// A wake is never shed, so it is placed regardless of the lane's depth.
    fn push_wake(&self, item: WakeItem) {
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

    /// The next job for `worker`, from its own lane only: a stealing scan
    /// would put every idle worker back on every other worker's lock. Wakes
    /// come out before dispatches. `None` once the queue is closed and the
    /// lane drained.
    fn pop(&self, worker: usize) -> Option<Job> {
        let lane = &self.lanes[worker % self.lanes.len()];
        let mut g = lane.inner.lock();
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

    /// Stops admission. `closed` and the lane are written under one lock, so
    /// a dispatch admitted before the close is still popped and served, and
    /// one arriving after it is handed back as a shed.
    fn close(&self) {
        for lane in self.lanes.iter() {
            lane.inner.lock().closed = true;
            lane.cv.notify_all();
        }
    }

    fn depth(&self) -> usize {
        self.lanes
            .iter()
            .map(|l| {
                let g = l.inner.lock();
                g.wakes.len() + g.dispatches.len()
            })
            .sum()
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

/// The cell's view of one resident instance. This is the bookkeeping half of a
/// slot; the other half is the reserved virtual address range the engine owns.
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
}

/// Routes completions from reactor threads onto the exec pool. Holds only weak
/// references, so a discarded instance's late completion is a no-op rather than
/// a resurrection.
struct CellWake {
    queue: Arc<WorkQueue>,
    index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>>,
    metrics: Arc<Metrics>,
}

impl WakeSink for CellWake {
    fn signal(&self, owner: InstanceId) {
        let slot = self.index.lock().get(&owner).and_then(Weak::upgrade);
        let Some(s) = slot else {
            // **The completion is already in the future table** — `complete`
            // wrote it Ready and pushed it onto the owner's `ready` queue
            // before this was called — and this is the only thing that would
            // have told a worker to come and get it. Dropping it here means
            // the result stays queued against an instance nobody will visit
            // again and the request that awaited it is never answered.
            //
            // For a genuinely discarded instance that is right: the router
            // holds only `Weak` references precisely so a late completion is a
            // no-op rather than a resurrection. For a live one it is a lost
            // request.
            // Nothing distinguished the two, and nothing counted either, so
            // the symptom was a request that simply stopped — `dispatches`
            // still claiming it was in flight, no trap, no refusal, no log.
            self.metrics.wakes_dropped.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                instance = %owner,
                "a completion arrived for an instance that is no longer resident; the wake \
                 is dropped and any dispatch awaiting it will not be answered"
            );
            return;
        };
        self.metrics.wakes.fetch_add(1, Ordering::Relaxed);
        self.queue.push_wake(WakeItem { slot: s });
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
    /// a guest has to hand the dispatch to a blocking task — guest execution
    /// runs on the exec pool and never on a reactor thread — and that needs an
    /// owned `Arc`. Weak, not strong, for the same reason the exec
    /// threads hold a `Weak`: a cell that keeps itself alive is a cell that
    /// leaks its threads in every test and every embedding.
    me: std::sync::OnceLock<Weak<Cell>>,
    rt: Arc<CellRuntime>,
    runtime: Option<tokio::runtime::Runtime>,
    queue: Arc<WorkQueue>,
    index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>>,
    /// **The tombstone ring.**
    ///
    /// A hard-killed instance's memory goes back to the kernel immediately —
    /// that part is not negotiable, it is arbitrary state — but the *slot*
    /// is kept for a while, holding nothing but its id and its tombstone
    /// flag. Without it, "a subsequent host call into it is refused" degrades
    /// to "the id is unknown", and those are very different answers to give an
    /// operator: the first says *this instance was killed and you may not talk
    /// to it*, the second says *never heard of it*, which is what a
    /// use-after-free looks like from the outside.
    ///
    /// It is worth the bytes because the engine will happily answer a call on
    /// a killed store — that has been observed — so the refusal has to come
    /// from a platform structure that outlives the instance.
    ///
    /// Bounded, because an unbounded record of every kill a long-lived cell
    /// ever took is a leak. The oldest entry is dropped when the ring is full,
    /// and a call for an id older than that gets `None` — the honest answer,
    /// since nothing about it is remembered.
    tombstoned: Mutex<std::collections::VecDeque<Arc<Slot>>>,
    modules: RwLock<HashMap<ModuleId, Arc<ModuleEntry>>>,
    residents: Mutex<HashMap<InstanceId, Resident>>,
    active: ShardedMap<InstanceId, ActiveRequest>,
    breaker: HangBreaker,
    metrics: Arc<Metrics>,
    hot: AtomicUsize,
    next_dispatch: AtomicU64,
    /// Tenants that have been unilaterally moved to a quarantine cell and may
    /// no longer be admitted here.
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
        let queue = Arc::new(WorkQueue::new(cfg.queue_depth_per_worker, cfg.exec_threads));
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
            modules: RwLock::new(HashMap::new()),
            // Handle 0 is reserved: a guest that reads an uninitialised field
            // and calls `socket_send(0, ..)` must get `NotFound`, not
            // somebody's chat room.
            residents: Mutex::new(HashMap::new()),
            active: ShardedMap::new(ACTIVE_SHARDS),
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

    /// Execution-pool threads run Hot dispatches as synchronous calls into the
    /// engine, and **the kernel preempts them**.
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

    /// The idle trajectory, running with no tenant involvement.
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
                    // One sweep's worth of blast radius. The idle trajectory
                    // runs with no tenant involvement, which is exactly what
                    // makes a panic here so expensive: nothing restarts this
                    // thread, and the loss is silent — captures
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
    /// **The job body is caught.** A host assertion inside a dispatch — the
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
    /// 1. The `hot` gauge keeps its increment. Nothing gates admission on it,
    ///    so this is telemetry drift and not a wedge.
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
                    Job::Wake(w) => cell.run_wake(w.slot),
                }));
                // Cleared either way, so a job that finished normally cannot
                // leave a stale id for the next panic to answer.
                let holding = IN_FLIGHT.replace(None);
                if ran.is_err() {
                    cell.note_caught_panic("dispatch");
                    cell.answer_after_panic(holding);
                }
            }
            // Released whether the job ran, panicked, or was dropped with the
            // cell, or the lane reads busy to every later placement.
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
    /// `Metrics` is the tenant-facing surface and this is not a fact about any
    /// tenant; it is a fact about this build.
    pub fn caught_panics(&self) -> u64 {
        self.caught_panics.load(Ordering::Relaxed)
    }

    // -----------------------------------------------------------------------
    // Registration and trust tiers
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

    pub fn metrics(&self) -> MetricsSnapshot {
        self.refresh_gauges();
        self.metrics.snapshot()
    }

    /// The live counters rather than a snapshot, for
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

    /// Clear the hang breaker for a module.
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
    /// The acquisition order is warm-parked, then pooled-fresh, then
    /// activate, so this is the difference between a request that pays for an
    /// activation and one that does not. Exposed because a *parked* instance
    /// and a *pooled* one are different things and the gauges cannot tell them
    /// apart: an instance is Parked from the moment the guest returns, but it
    /// is not acquirable until `finish_and_release` puts it back — a window
    /// the response has already left through, because the cell responds before
    /// it releases.
    pub fn idle_instances(&self, module: &ModuleId) -> Option<(usize, usize)> {
        self.modules
            .read()
            .get(module)
            .map(|e| e.pool.idle_counts())
    }

    /// Queued work, across every lane.
    pub fn queue_depth(&self) -> usize {
        self.queue.depth()
    }
    pub fn hot_count(&self) -> usize {
        self.hot.load(Ordering::Relaxed)
    }
    pub fn futures(&self) -> &Arc<FutureTable> {
        &self.rt.futures
    }
    /// The cell's reactor. Reactor threads own all I/O, so anything
    /// that touches the network — the metrics endpoint, the proxy's listener —
    /// belongs here rather than on an execution-pool thread.
    pub fn reactor(&self) -> &tokio::runtime::Handle {
        &self.rt.reactor
    }

    // -----------------------------------------------------------------------
    // Admission
    // -----------------------------------------------------------------------

    /// Submit a top-level work item and block until the response is final.
    ///
    /// "Final" means Done or a fault — a dispatch that parks and is later woken
    /// keeps this call outstanding, because one request is a lifecycle spanning
    /// many slices.
    pub fn dispatch_blocking(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        timeout: Duration,
    ) -> crate::Result<DispatchResult> {
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
        });
        if self.queue.push_dispatch(job).is_err() {
            // Overflow queues at admission; the cell sheds rather than
            // saturating.
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
    /// This is the shape a parked-density measurement needs: many dispatches
    /// that *park* and stay parked. `dispatch_blocking` would hold a thread per
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
    /// offering sustained load: it is one allocation per request on the
    /// caller's path, and — worse — leaves the caller holding one receiver per
    /// outstanding request to poll. A load generator built that way measures
    /// itself.
    ///
    /// The result carries [`DispatchResult::dispatch_id`], so a caller
    /// draining one shared channel can match each completion to its request.
    ///
    /// Returns `false` if the cell shed at admission. **Nothing is sent
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
        // Taken above the breaker check, so the check's lock is inside the
        // interval `ActiveRequest::started_ns` measures.
        let enqueued_ns = crate::mono_ns();
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
            if responder
                .send(DispatchResult::breaker_open(dispatch_id))
                .is_err()
            {
                tracing::debug!(
                    dispatch_id,
                    "the caller stopped waiting before its breaker refusal was answered"
                );
            }
            return true;
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder,
            enqueued_ns,
        });
        match self.queue.push_dispatch(job) {
            Ok(()) => true,
            Err(_) => {
                self.metrics.shed.fetch_add(1, Ordering::Relaxed);
                if let Some(m) = self.meters(module) {
                    m.record_shed();
                }
                false
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
    /// its owning instance and enqueue the wake.
    ///
    /// Public because the reactor is not the only completer — an alarm from
    /// the control plane, a socket frame at the proxy, a DI reply and a test
    /// all arrive this way.
    pub fn complete_future(&self, id: u64, result: wisp_abi::result::ResultRecord) {
        // Through the runtime rather than inline, because the vtable's own
        // `settle` is the *other* door and is the one every guest-awaited
        // future comes through; one path serves both.
        self.rt.settle_future(id, result);
    }

    /// Build a plain HTTP dispatch record with this app's budget.
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
    // The request lifecycle
    // -----------------------------------------------------------------------

    fn run_dispatch(self: &Arc<Self>, job: DispatchJob) {
        let did = job.rec.dispatch_id;
        let entry = self.modules.read().get(&job.module).cloned();
        let Some(entry) = entry else {
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
            // carry what earlier ones spent, so the budget and the hard
            // deadline bind the dispatch rather than each wake.
            spent_ns: 0,
        };
        // An instance may have several dispatches in flight only for
        // read-only Handler reuse; a durable instance takes strictly one at a
        // time. A Handler never reaches this branch — a parked instance is not
        // in the pool, so a second dispatch gets a different one — but a DI is
        // addressed by identity and can. Refuse rather than overwrite the
        // in-flight request. Ordering and serialization of DI messages is the
        // single-writer inbox's job, which lives in `wisp-durable`.
        let already_in_flight = self.active.contains_key(&slot.id);
        if already_in_flight {
            // Counted, because the only other count of this refusal lives in
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
                body: "durable instance already has a dispatch in flight"
                    .as_bytes()
                    .to_vec(),
                ..DispatchResult::shed(did)
            });
            return;
        }
        // Every dispatch carries an implicit cancellation future, and the host
        // cancels by completing it. It is allocated here, before the guest is
        // entered, because the epoch callback that completes it at the budget
        // runs with the guest on the stack and could not allocate one then.
        //
        // A guest that never awaits it pays nothing but a table entry, which
        // `end_dispatch` reaps. A guest that does await it observes a tripped
        // token at its next wake — the only channel there is, because
        // cancellation is deliberately not an entrancy.
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
                started_ns: job.enqueued_ns,
                cpu_ns: 0,
                wall_ns: 0,
                slices: 0,
                acquired,
                budget,
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
                    // The reentrancy rule refusing a second call. For a
                    // Durable Instance that is "strictly one dispatch at a
                    // time"; the caller must retry.
                    //
                    // The tombstone refusal arrives at the same door and is a
                    // different fact — "this instance was killed and you may
                    // not talk to it", not "come back in a moment" — so it is
                    // counted separately. Folding it into the reentrancy
                    // counter left `tombstone_refusals`, the negative control
                    // for the tombstone rule, permanently at zero.
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

        // A completion can land while a dispatch is Hot; the reentrancy rule
        // made it wait, so if one did, hand the instance to the wake lane
        // rather than leaving a result queued against a parked instance nobody
        // will visit again.
        if !self.settle(&slot, &entry, outcome) {
            self.queue_wake(slot);
        }
    }

    /// Put an instance in the wake lane.
    fn queue_wake(&self, slot: Arc<Slot>) {
        // Counted because it is otherwise invisible: this path does not go
        // through `CellWake::signal`, so it bumps neither `wakes` nor anything
        // else. See `Metrics::in_cell_wakes`.
        self.metrics.in_cell_wakes.fetch_add(1, Ordering::Relaxed);
        self.queue.push_wake(WakeItem { slot });
    }

    /// One woken slice, and every further slice the instance can take without
    /// going back through a lane.
    fn run_wake(self: &Arc<Self>, slot: Arc<Slot>) {
        // A wake runs an existing request, which is therefore already in
        // `Cell::active` and already answerable. The id comes from the
        // instance's own context, which `begin_dispatch` set and every slice
        // of this request carries. See `Cell::answer_after_panic`.
        IN_FLIGHT.set(Some((slot.id, slot.ctx.dispatch_id())));
        let module = self
            .residents
            .lock()
            .get(&slot.id)
            .map(|r| r.module.clone());
        let Some(module) = module else { return };
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
                    // A previous call into this instance has not returned.
                    // This is the reentrancy rule doing its job — the result
                    // stays queued and the in-flight call drains it when it
                    // finishes.
                    self.metrics
                        .reentrancy_refusals
                        .fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(e @ crate::instances::EnterError::Tombstoned) => {
                    // A wake queued for an instance that was tombstoned before
                    // the wake lane reached it. Refused, and counted.
                    self.count_enter_refusal(e);
                    return;
                }
                Err(_) => return,
            };
            let Some((fid, res)) = self.rt.futures.take_ready(slot.id) else {
                // Several completions for one instance produce several wakes,
                // and the first one's loop drains all of them; the rest arrive
                // to an empty table.
                return;
            };
            // The budget and the hard deadline bind the **dispatch**, so a
            // wake resumes with what earlier slices already burned. Without
            // this a guest could park and be woken indefinitely, each slice
            // getting a fresh budget and the hard deadline never arriving —
            // and a guest unwinding inside grace would get its grace window
            // back on every await, so pinning the unsafe-to-trap bit would buy
            // an unbounded extension rather than one grace window.
            let budget = self
                .active
                .with_mut(&slot.id, |a| a.budget.after(a.cpu_ns))
                .unwrap_or_else(CpuBudget::http);

            // A wake is a new slice, so the coarse clock advances here and
            // nowhere else.
            slot.ctx
                .begin_slice(crate::now_ms(), slot.ctx.dispatch_id());

            let t0 = Instant::now();
            self.hot.fetch_add(1, Ordering::Relaxed);
            let outcome = guard.instance().wake(fid, &res, budget);
            self.hot.fetch_sub(1, Ordering::Relaxed);
            // Same ordering as the dispatch path, and for the same reason: a
            // resumed slice that traps must not leave the sandbox enterable
            // while `settle` is still on its way to the tombstone.
            if ends_in_tombstone(&outcome, &slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            self.metrics.wake_us.record(t0.elapsed().as_micros() as u64);

            if self.settle(&slot, &entry, outcome) {
                return;
            }
        }
    }

    /// The poll-status switch. Returns true when the instance is no longer
    /// available for more wakes in this loop.
    fn settle(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        out: DispatchOutcome,
    ) -> bool {
        // Record the Hot interval. Nothing else is ever recorded — and on a kill
        // of either phase that interval *includes the overage*, because the
        // meter reads the thread clock and does not know what a budget is.
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
        // import that returns a bare `i64` — the pending-futures cap, a
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
                // A completion may have landed while we were Hot; the
                // reentrancy rule made it wait, so drain it now.
                !self.rt.futures.has_ready(slot.id)
            }
            Completion::Done => {
                // A guest may return `done` with a body-stream handle and
                // keep producing chunks through write futures under
                // `wait_until` semantics. An unfinished streaming response
                // must therefore stay in the response table — taking it here
                // would make every subsequent `resp_write` fail with
                // `NoSuchHandle`, which is a silent truncation of the tenant's
                // response.
                //
                // **Not wired:** `DispatchResult` carries a complete body, not
                // a stream, so the caller receives the bytes accumulated up to
                // `done`. Chunks written afterwards accumulate on the instance
                // and are readable via `Cell::response_snapshot`, but nothing
                // pipes them to the requester. Streaming to the client is
                // `wisp-proxy`'s half and is not built.
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
                let outstanding = self.rt.futures.wait_until_outstanding(slot.id);
                let wait_until = outstanding || streaming_open;

                let (status, headers, body) = match &response {
                    Some(r) => (r.head.status, r.head.headers.clone(), r.body()),
                    None => (204, vec![], Vec::new()),
                };
                let result = self.build_result(slot, status, headers, body, completion, None);
                self.respond(slot, result);

                match decide(lifecycle, completion, wait_until) {
                    RequestEnd::RetainedForWaitUntil => {
                        // The instance lives past the response and, under
                        // `fresh`, the reset is still owed.
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
            // The CLEAN kill: the dispatch entered grace and returned inside
            // it. The request still fails — a kill is a kill, and the tenant
            // still sees `budget_exceeded` — but three things that are true of
            // a hard kill are false here, and all three are the point:
            //
            //   * the instance is NOT tombstoned. `finally` ran, its state is
            //     consistent, and its slot is reusable (`decide` resets it);
            //   * it does NOT count toward the hang breaker: a clean kill is
            //     the budget working as designed;
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
                    "clean kill: dispatch unwound inside its grace window"
                );
                let msg = format!(
                    "budget_exceeded: cpu budget elapsed; the dispatch unwound cleanly inside its \
                     grace window and was recorded {:.3} ms of overage",
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
                    // Only HARD kills trip the hang breaker.
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
                // **The tombstone.** Set on the slot before anything else
                // touches it, because the engine will not do it for us — a
                // killed instance has been observed answering a later exported
                // call correctly, globals intact. From here `slot.enter()`
                // refuses, capture is refused, and reuse is impossible.
                slot.tombstone();
                // The identity `tombstones == hard_kills + traps` is
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
                // A trap is never retried on the same memory, and the slot is
                // reset before reuse — for a tombstone "reset" means the whole
                // activation goes back to the kernel, not a re-seal over
                // memory a trap left mid-write.
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
        let (dispatch_id, cpu_ns, wall_ns, slices, acquired) = self
            .active
            .with(&slot.id, |r| {
                (r.dispatch_id, r.cpu_ns, r.wall_ns, r.slices, r.acquired)
            })
            .unwrap_or((0, 0, 0, 0, "unknown"));
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
        }
    }

    fn respond(&self, slot: &Arc<Slot>, result: DispatchResult) {
        let tx = self
            .active
            .with_mut(&slot.id, |a| a.responder.take())
            .flatten();
        if let Some(tx) = tx {
            if tx.send(result).is_err() {
                tracing::debug!(
                    instance = %slot.id,
                    "the caller stopped waiting before its dispatch was answered"
                );
            }
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
                .record(crate::mono_ns().saturating_sub(r.started_ns) / 1_000);
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
    /// The three increments are the ones the identity
    /// `tombstones == hard_kills + traps` is built out of. Leaving them out
    /// would make a tombstone the cell definitely took invisible to
    /// [`Metrics::tombstone_invariant`] in the direction that *fails* the
    /// check, which is at least loud — but `traps` would also under-report,
    /// and the tombstone count is the one a blast-radius table reads.
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
             discarded and the slot keeps refusing"
        );
    }

    /// Keep a hard-killed slot addressable so a later call gets a refusal
    /// rather than a shrug. See the note on [`Cell::tombstoned`].
    fn remember_tombstone(&self, slot: &Arc<Slot>) {
        /// Enough to cover the burst a breaker takes to notice, and small
        /// enough that the ring costs far less than one instance.
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

    /// Every instance this cell currently remembers tombstoning. The negative
    /// control reads it: a tombstone rule with nothing ever in here and no
    /// failing probe is unfalsifiable.
    pub fn tombstoned_instances(&self) -> Vec<InstanceId> {
        self.tombstoned.lock().iter().map(|s| s.id).collect()
    }

    /// Attribute one `Slot::enter` refusal to the counter that names it.
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

    /// Call into an instance by id, through the cell's front door.
    ///
    /// [`Cell::slot`] answers from the tombstone ring on purpose, so a
    /// kill-then-call gets an instance that **refuses** rather than a `None`
    /// that reads as "no such instance". This is that refusal *counted*:
    /// `tombstone_refusals` is the negative control for the tombstone rule,
    /// and a control nothing ever increments cannot fail, which makes a green
    /// dashboard unfalsifiable.
    ///
    /// The closure shape is not decoration: [`crate::instances::CallGuard`]
    /// borrows the slot, so handing one out would mean handing out the
    /// lifetime too. Going through `Slot::enter` rather than re-deriving the
    /// three refusals from `is_tombstoned`/`is_busy`/`state` keeps the
    /// refusal unrepresentable rather than merely discouraged — this is the
    /// same door every dispatch uses, with a counter on it.
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
        let mut residents = self.residents.lock();
        residents.entry(slot.id).or_insert_with(|| Resident {
            slot: slot.clone(),
            module: entry.activator.module().clone(),
        });
        drop(residents);
        Ok((slot, src))
    }

    // -----------------------------------------------------------------------
    // Inspection and forced transitions (tests, drills, `wisp dev`)
    // -----------------------------------------------------------------------

    /// Look up an instance by id.
    ///
    /// Falls back to the tombstone ring, so a kill-then-call gets an
    /// instance that **refuses** rather than a `None` that reads as "no such
    /// instance". Linear over the ring, and only on the miss path.
    pub fn slot(&self, id: InstanceId) -> Option<Arc<Slot>> {
        if let Some(s) = self.residents.lock().get(&id).map(|r| r.slot.clone()) {
            return Some(s);
        }
        self.tombstoned.lock().iter().find(|s| s.id == id).cloned()
    }

    // -----------------------------------------------------------------------
    // Identity lookups
    //
    // These are public because the questions they answer are asked from
    // outside the cell and answering them by guessing is worse than not
    // answering: the proxy's thundering-herd guard keys off residency, and an
    // admin surface that reports tiers by identity has to get the identity
    // from somewhere.
    // -----------------------------------------------------------------------

    /// Whether a version's template is registered on this cell. "Any cell with
    /// the template cached", asked rather than assumed.
    pub fn has_module(&self, module: &ModuleId) -> bool {
        self.modules.read().contains_key(module)
    }

    /// The tenant a registered module belongs to.
    pub fn module_tenant(&self, module: &ModuleId) -> Option<String> {
        self.modules.read().get(module).map(|m| m.tenant.clone())
    }

    // -----------------------------------------------------------------------
    // Socket handles
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
    /// The bytes a streaming response has accumulated so far. See the note in
    /// `settle` about what is not wired to the requester.
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

    /// Run one pass of the idle trajectory synchronously.
    pub fn sweep_now(self: &Arc<Self>) {}

    // -----------------------------------------------------------------------
    // Quarantine migration
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
                cpu_budget_ms: 30_000,
                now_ms: 0,
            },
            responder,
            enqueued_ns: crate::mono_ns(),
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
        WakeItem { slot: slot.clone() }
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

    #[test]
    fn every_worker_has_a_lane_of_the_configured_depth() {
        let q = WorkQueue::new(128, 8);
        assert_eq!(q.lanes.len(), 8, "one lane per worker");
        assert_eq!(q.cap, 128, "a lane holds the configured depth, undivided");

        let q = WorkQueue::new(4, 64);
        assert_eq!(q.lanes.len(), 64, "one lane per worker");
        assert_eq!(
            q.cap, 4,
            "64 workers must not turn a depth of 4 into a depth of 1"
        );
    }

    #[test]
    fn a_lane_sheds_the_dispatch_past_its_depth() {
        let q = WorkQueue::new(4, 1);
        for i in 0..4 {
            assert!(
                q.push_dispatch(job()).is_ok(),
                "job {i} is inside the depth"
            );
        }
        assert!(
            q.push_dispatch(job()).is_err(),
            "the fifth dispatch on a lane of depth four must be handed back"
        );
        assert!(
            matches!(q.pop(0), Some(Job::Dispatch(_))),
            "draining makes room"
        );
        q.job_done(0);
        assert!(
            q.push_dispatch(job()).is_ok(),
            "the shed is a depth limit, not a one-way door"
        );
        q.close();
    }

    #[test]
    fn a_wake_is_never_shed() {
        let q = WorkQueue::new(1, 1);
        assert!(q.push_dispatch(job()).is_ok(), "the one slot");
        assert!(q.push_dispatch(job()).is_err(), "the lane is full");
        let slot = a_slot();
        q.push_wake(wake_of(&slot));
        assert!(
            matches!(q.pop(0), Some(Job::Wake(_))),
            "a wake pushed onto a full lane must still be queued, and ahead of the dispatch"
        );
        q.close();
    }

    /// Checked statistically: with one loaded lane and the rest idle, a
    /// placer that reads load lands on the loaded one only when both samples
    /// drew it, and a blind one lands there one time in N.
    #[test]
    fn power_of_two_avoids_the_loaded_lane() {
        let q = WorkQueue::new(4096, 8);
        q.lanes[3].load.store(1_000, Ordering::Relaxed);

        let mut hit_deep = 0;
        const N: usize = 20_000;
        for _ in 0..N {
            if q.pick() == 3 {
                hit_deep += 1;
            }
        }
        // Blind: about N/8. Both samples on lane 3: about N/64.
        assert!(
            hit_deep < N / 20,
            "the loaded lane was chosen {hit_deep} times in {N}; the placer is not reading load"
        );
        let seen: std::collections::HashSet<usize> = (0..2_000).map(|_| q.pick()).collect();
        assert!(
            seen.len() >= 6,
            "the sampler must reach most lanes, saw {}",
            seen.len()
        );
    }

    #[test]
    fn a_lane_keeps_counting_the_job_its_worker_is_still_running() {
        let q = WorkQueue::new(8, 1);
        assert!(q.push_dispatch(job()).is_ok(), "one job");
        let load = || q.lanes[0].load.load(Ordering::Relaxed);
        assert_eq!(load(), 1, "a queued dispatch is load");

        let taken = q.pop(0).expect("the job");
        assert_eq!(
            load(),
            1,
            "the worker is inside the guest with this job; a placer that reads zero here sends \
             it another"
        );
        drop(taken);
        q.job_done(0);
        assert_eq!(
            load(),
            0,
            "the load is released when the job finishes, not when it is taken"
        );
    }

    #[test]
    fn queued_wakes_count_toward_the_lane_that_is_holding_them() {
        let q = WorkQueue::new(8, 4);
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
    }

    #[test]
    fn a_wake_is_popped_before_a_dispatch() {
        let slot = a_slot();
        let q = WorkQueue::new(8, 1);
        assert!(q.push_dispatch(job()).is_ok(), "the dispatch");
        q.push_wake(wake_of(&slot));
        assert!(
            matches!(q.pop(0), Some(Job::Wake(_))),
            "the wake queued second must come out first"
        );
        assert!(
            matches!(q.pop(0), Some(Job::Dispatch(_))),
            "and the dispatch after it"
        );
        q.close();
    }

    #[test]
    fn closing_the_queue_releases_parked_workers() {
        let q = Arc::new(WorkQueue::new(64, 4));
        let hs: Vec<_> = (0..4)
            .map(|w| {
                let q = q.clone();
                std::thread::spawn(move || q.pop(w))
            })
            .collect();
        // A worker with an empty lane is on its condition variable by the
        // time it can be observed holding nothing, so the close must reach it
        // there or the join below hangs.
        std::thread::sleep(Duration::from_millis(20));
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

    #[test]
    fn a_dispatch_admitted_before_the_close_is_still_served() {
        let q = WorkQueue::new(8, 1);
        assert!(q.push_dispatch(job()).is_ok(), "admitted");
        q.close();
        assert!(
            matches!(q.pop(0), Some(Job::Dispatch(_))),
            "the close must not strand a dispatch the caller was told was admitted"
        );
        assert!(q.pop(0).is_none(), "and then the lane reports closure");
        assert!(
            q.push_dispatch(job()).is_err(),
            "a dispatch after the close is handed back as a shed"
        );
    }
}

/// Cell-level tests: what survives a host panic.
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
    /// The one that has actually fired is the tombstone assertion, which is a
    /// `panic!` on purpose. Any other `assert!` or slice index in this crate
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
        fn template_image(&self) -> Arc<wisp_engine::MemoryImage> {
            Arc::new(wisp_engine::MemoryImage::from_dense(&self.template))
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
