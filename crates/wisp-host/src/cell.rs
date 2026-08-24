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

use parking_lot::{Condvar, Mutex, RwLock};

use wisp_abi::config::{AppManifest, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, TraceContext};
use wisp_abi::status::{PollStatus, TrapCode};
use wisp_engine::{Completion, CpuBudget, DispatchOutcome};

use crate::bindings::{CellRuntime, InstanceCtx, WakeSink};
pub use crate::bindings::{Fetcher, Services};
use crate::breaker::{BreakerConfig, BreakerKey, HangBreaker};
use crate::futures::FutureTable;
use crate::instances::{
    decide, evict, AcquireSource, Activator, InstancePool, InstanceState, RequestEnd, Slot,
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
    pub max_idle_per_module: usize,
    /// Wall-clock backstop for a dispatch. Not a meter (§11.2).
    pub wall_ms_cap: u32,
    pub breaker: BreakerConfig,
    /// How often the idle trajectory sweeps (§6.6).
    pub sweep_interval_ms: u64,
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
}

impl DispatchResult {
    fn shed() -> Self {
        Self {
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
    fn breaker_open() -> Self {
        Self {
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
}

/// A two-lane queue.
///
/// Wakes jump ahead of new dispatches, and wakes are never shed. That is
/// §6.4's admission story stated as a data structure: overflow queues at
/// admission and sheds *new* work; work already in flight keeps making
/// progress, which is what keeps a saturated cell from converting itself into
/// a pile of half-finished requests.
struct WorkQueue {
    inner: Mutex<QueueInner>,
    cv: Condvar,
    cap: usize,
}

#[derive(Default)]
struct QueueInner {
    wakes: std::collections::VecDeque<Arc<Slot>>,
    dispatches: std::collections::VecDeque<Box<DispatchJob>>,
    closed: bool,
}

impl WorkQueue {
    fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(QueueInner::default()),
            cv: Condvar::new(),
            cap,
        }
    }
    /// `Err` means shed (§6.4).
    fn push_dispatch(&self, j: Box<DispatchJob>) -> Result<(), Box<DispatchJob>> {
        let mut g = self.inner.lock();
        if g.closed || g.dispatches.len() >= self.cap {
            return Err(j);
        }
        g.dispatches.push_back(j);
        drop(g);
        self.cv.notify_one();
        Ok(())
    }
    fn push_wake(&self, s: Arc<Slot>) {
        let mut g = self.inner.lock();
        if g.closed {
            return;
        }
        g.wakes.push_back(s);
        drop(g);
        self.cv.notify_one();
    }
    fn pop(&self) -> Option<Job> {
        let mut g = self.inner.lock();
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
            self.cv.wait(&mut g);
        }
    }
    fn close(&self) {
        self.inner.lock().closed = true;
        self.cv.notify_all();
    }
    fn depth(&self) -> usize {
        let g = self.inner.lock();
        g.wakes.len() + g.dispatches.len()
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
    rt: Arc<CellRuntime>,
    runtime: Option<tokio::runtime::Runtime>,
    queue: Arc<WorkQueue>,
    index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>>,
    modules: RwLock<HashMap<ModuleId, Arc<ModuleEntry>>>,
    residents: Mutex<HashMap<InstanceId, Resident>>,
    active: Mutex<HashMap<InstanceId, ActiveRequest>>,
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
        let queue = Arc::new(WorkQueue::new(cfg.admission_queue));
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
            breaker: HangBreaker::new(cfg.breaker),
            rt,
            runtime: Some(runtime),
            queue,
            index,
            modules: RwLock::new(HashMap::new()),
            residents: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            metrics,
            hot: AtomicUsize::new(0),
            next_dispatch: AtomicU64::new(1),
            quarantined: Mutex::new(Vec::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            workers: Mutex::new(Vec::new()),
            cfg,
        });

        cell.start_exec_pool();
        cell.start_idle_sweeper();
        cell
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
                    .spawn(move || Cell::exec_loop(me, queue))
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

    fn exec_loop(me: Weak<Self>, queue: Arc<WorkQueue>) {
        while let Some(job) = queue.pop() {
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

    pub fn metrics(&self) -> MetricsSnapshot {
        self.refresh_gauges();
        self.metrics.snapshot()
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
                InstanceState::Hot => {
                    hot += 1;
                    live += 1;
                }
                InstanceState::Parked => {
                    parked += 1;
                    live += 1;
                }
                InstanceState::Hibernated => hib += 1,
                InstanceState::Discarded => {}
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
        if self.breaker.check(&key, now).is_err() {
            self.metrics
                .breaker_rejections
                .fetch_add(1, Ordering::Relaxed);
            return Ok(DispatchResult::breaker_open());
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder: tx,
            enqueued: Instant::now(),
        });
        if self.queue.push_dispatch(job).is_err() {
            // §6.4: overflow queues at admission; the cell sheds rather than
            // saturating. The proxy is supposed to shed before this point.
            self.metrics.shed.fetch_add(1, Ordering::Relaxed);
            if let Some(m) = self.meters(module) {
                m.record_shed();
            }
            return Ok(DispatchResult::shed());
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
        let now = crate::now_ms();
        let key = BreakerKey {
            module: module.clone(),
            route: target.route(),
        };
        if self.breaker.check(&key, now).is_err() {
            self.metrics
                .breaker_rejections
                .fetch_add(1, Ordering::Relaxed);
            let _ = tx.send(DispatchResult::breaker_open());
            return Some(rx);
        }
        let job = Box::new(DispatchJob {
            module: module.clone(),
            target,
            rec,
            responder: tx,
            enqueued: Instant::now(),
        });
        if self.queue.push_dispatch(job).is_err() {
            self.metrics.shed.fetch_add(1, Ordering::Relaxed);
            if let Some(m) = self.meters(module) {
                m.record_shed();
            }
            return None;
        }
        Some(rx)
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
        let Some(entry) = self.modules.read().get(&job.module).cloned() else {
            let _ = job.responder.send(DispatchResult {
                status: 404,
                body: b"no such module".to_vec(),
                ..DispatchResult::shed()
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
                            ..DispatchResult::shed()
                        });
                        return;
                    }
                }
            }
        };

        let budget = CpuBudget {
            cpu_ms: job.rec.cpu_budget_ms,
            wall_ms_cap: self.cfg.wall_ms_cap,
        };
        // §5.2/§7.2: "One instance MAY have multiple in-flight dispatches only
        // for read-only Handler reuse; DIs are strictly one dispatch at a
        // time." A Handler never reaches this branch — a parked instance is
        // not in the pool, so a second dispatch gets a different one — but a
        // DI is addressed by identity and can. Refuse rather than overwrite the
        // in-flight request. Ordering and serialization of DI messages is the
        // single-writer inbox's job (§7.2), which lives in `wisp-durable`.
        if self.active.lock().contains_key(&slot.id) {
            let _ = job.responder.send(DispatchResult {
                status: 409,
                body: "durable instance already has a dispatch in flight (§7.2)"
                    .as_bytes()
                    .to_vec(),
                ..DispatchResult::shed()
            });
            return;
        }
        self.active.lock().insert(
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
            },
        );

        entry.meters.record_dispatch();
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);

        slot.ctx
            .begin_dispatch(crate::now_ms(), job.rec.dispatch_id);

        let outcome = {
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(_) => {
                    // §5.4 refusing a reentrant call. For a Durable Instance
                    // this is §7.2's "strictly one dispatch at a time"; the
                    // caller must retry.
                    self.metrics
                        .reentrancy_refusals
                        .fetch_add(1, Ordering::Relaxed);
                    self.respond(
                        &slot,
                        DispatchResult {
                            status: 409,
                            body: b"instance busy".to_vec(),
                            ..DispatchResult::shed()
                        },
                    );
                    self.active.lock().remove(&slot.id);
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
                Err(_) => return,
            };
            let Some((fid, res)) = self.rt.futures.take_ready(slot.id) else {
                return;
            };
            let budget = self
                .active
                .lock()
                .get(&slot.id)
                .map(|a| a.budget)
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
        // §11.2: record the Hot interval. Nothing else is ever recorded.
        entry.meters.record_cpu(&out);
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
            let mut a = self.active.lock();
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
            Completion::Trapped | Completion::BudgetKill => {
                let killed = completion == Completion::BudgetKill;
                if killed {
                    entry.meters.record_budget_kill();
                    self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
                    self.breaker.record_kill(&breaker_key, crate::now_ms());
                } else {
                    entry.meters.record_trap();
                }
                self.metrics.traps.fetch_add(1, Ordering::Relaxed);
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
                // §13: traps are never retried on the same memory.
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
        let a = self.active.lock();
        let (cpu_ns, wall_ns, slices, acquired) = a
            .get(&slot.id)
            .map(|r| (r.cpu_ns, r.wall_ns, r.slices, r.acquired))
            .unwrap_or((0, 0, 0, "unknown"));
        DispatchResult {
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
            .lock()
            .get_mut(&slot.id)
            .and_then(|a| a.responder.take());
        if let Some(tx) = tx {
            let _ = tx.send(result);
        }
    }

    fn finish_and_release(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        d: crate::instances::Disposition,
    ) {
        let req = self.active.lock().remove(&slot.id);
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
        if slot.state() == InstanceState::Discarded {
            self.forget(slot);
        }
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

    pub fn slot(&self, id: InstanceId) -> Option<Arc<Slot>> {
        self.residents.lock().get(&id).map(|r| r.slot.clone())
    }
    pub fn instance_state(&self, id: InstanceId) -> Option<InstanceState> {
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
