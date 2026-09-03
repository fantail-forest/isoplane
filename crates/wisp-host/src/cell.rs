//! The cell: reactor threads that own all I/O, an execution pool that runs
//! Hot dispatches as synchronous calls into the engine, the instance
//! registries, and the idle trajectory. There is no scheduler: the kernel
//! preempts a CPU-bound tenant on its own pool thread, and the cell controls
//! admission with a bounded lane per worker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use wisp_abi::config::{AppManifest, HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, TraceContext};
use wisp_abi::status::{PollStatus, TrapCode};
use wisp_engine::{Completion, CpuBudget, DispatchOutcome, KillPhase};

use crate::bindings::Tenancy;
use crate::bindings::{CellRuntime, InstanceCtx, WakeSink};
pub use crate::bindings::{Fetcher, Services};
use crate::breaker::{BreakerConfig, BreakerKey, HangBreaker};
use crate::futures::FutureTable;
use crate::instances::{
    decide, evict, AcquireSource, Activator, EnterError, InstancePool, LifecycleTier, RequestEnd,
    Slot,
};
use crate::meters::Meters;
use crate::metrics::{Metrics, MetricsSnapshot};
use crate::sharded_map::ShardedMap;

mod hibernate;
mod queue;
use crate::{InstanceId, ModuleId};
use queue::{DispatchJob, Job, WakeItem, WorkQueue};

#[derive(Debug, Clone)]
pub struct CellConfig {
    pub name: String,
    /// The bound on concurrently Hot dispatches. Defaults to four times the
    /// logical cpus.
    pub exec_threads: usize,
    pub reactor_threads: usize,
    /// Dispatches one worker's lane holds before the cell sheds. Each
    /// execution thread has its own lane, so the cell as a whole holds
    /// `exec_threads` times this.
    pub queue_depth_per_worker: usize,
    pub max_idle_per_module: usize,
    /// Wall-clock backstop for a dispatch; reported separately from active execution time.
    pub wall_ms_cap: u32,
    pub breaker: BreakerConfig,
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

#[derive(Debug, Clone)]
pub enum Target {
    /// Any instance of the module.
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
    /// Echoed so a caller draining one channel for many dispatches can match
    /// a result to its request; completions arrive out of order.
    pub dispatch_id: u64,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub completion: Completion,
    /// The recorded meter.
    pub cpu_ns: u64,
    /// Telemetry only.
    pub wall_ns: u64,
    pub trap: Option<String>,
    pub instance: InstanceId,
    /// How the dispatch got its instance; see `AcquireSource::label`.
    pub acquired: &'static str,
    /// One for a dispatch that never parked, more if it awaited.
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

/// Decided while the guard is still held, because it chooses between
/// releasing the guard and destroying it, and a guard released before its
/// tombstone lets another thread enter memory a trap abandoned. A latched trap
/// counts: `settle` promotes it to `Completion::Trapped`.
fn ends_in_tombstone(out: &DispatchOutcome, slot: &Arc<Slot>) -> bool {
    matches!(out.completion, Completion::Trapped | Completion::BudgetKill)
        || (matches!(out.completion, Completion::Done | Completion::Parked) && slot.ctx.has_trap())
}

thread_local! {
    /// The request this execution thread is running, for the panic handler
    /// in `Cell::exec_loop`, which has to answer a caller whose job unwound.
    /// The dispatch id is needed because `finish_and_release` hands the
    /// instance back before the job returns, so a panic after that point
    /// names a slot another worker may already hold for a new request.
    static IN_FLIGHT: std::cell::Cell<Option<(InstanceId, u64)>> =
        const { std::cell::Cell::new(None) };
}

/// Shards for `Cell::active`, above the default pool size so two threads are
/// not guaranteed to share one.
const ACTIVE_SHARDS: usize = 256;

struct ModuleEntry {
    tenant: String,
    limits: Limits,
    manifest: AppManifest,
    activator: Arc<dyn Activator>,
    pool: InstancePool,
    meters: Arc<Meters>,
}

/// The cell's bookkeeping for one resident instance.
struct Resident {
    slot: Arc<Slot>,
    module: ModuleId,
}

impl Resident {}

struct ActiveRequest {
    dispatch_id: u64,
    lifecycle: HandlerLifecycle,
    module: ModuleId,
    route: String,
    responder: Option<std::sync::mpsc::SyncSender<DispatchResult>>,
    /// `None` when the clock could not be read at admission.
    started_ns: Option<u64>,
    cpu_ns: u64,
    wall_ns: u64,
    slices: u32,
    acquired: &'static str,
    budget: CpuBudget,
}

/// Routes completions onto the exec pool. Weak references, so a discarded
/// instance's late completion is a no-op rather than a resurrection.
struct CellWake {
    queue: Arc<WorkQueue>,
    index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>>,
    metrics: Arc<Metrics>,
}

impl WakeSink for CellWake {
    fn signal(&self, owner: InstanceId) {
        let slot = self.index.lock().get(&owner).and_then(Weak::upgrade);
        let Some(s) = slot else {
            // The result is already Ready in the future table; for a live
            // instance this is a request that is never answered.
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

pub struct Cell {
    pub cfg: CellConfig,
    /// Weak, like the exec threads' reference: a cell that keeps itself alive
    /// leaks its threads in every embedding. `Dispatcher` methods take `&self`
    /// and hand the dispatch to a blocking task, which needs an owned `Arc`.
    me: std::sync::OnceLock<Weak<Cell>>,
    rt: Arc<CellRuntime>,
    runtime: Option<tokio::runtime::Runtime>,
    queue: Arc<WorkQueue>,
    index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>>,
    /// A bounded ring of killed slots, kept so a later call gets "this
    /// instance was killed" rather than "never heard of it". The engine would
    /// answer a call on a killed store, so the refusal has to outlive the
    /// instance.
    tombstoned: Mutex<std::collections::VecDeque<Arc<Slot>>>,
    modules: RwLock<HashMap<ModuleId, Arc<ModuleEntry>>>,
    residents: Mutex<HashMap<InstanceId, Resident>>,
    active: ShardedMap<InstanceId, ActiveRequest>,
    breaker: HangBreaker,
    metrics: Arc<Metrics>,
    hot: AtomicUsize,
    next_dispatch: AtomicU64,
    /// Tenants moved to a quarantine cell, no longer admitted here.
    quarantined: Mutex<Vec<String>>,
    shutdown: Arc<AtomicBool>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    caught_panics: AtomicU64,
}

impl Cell {
    /// Fails if the reactor or a pool thread cannot be started.
    pub fn new(cfg: CellConfig, services: Arc<Services>) -> crate::Result<Arc<Self>> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(cfg.reactor_threads)
            .thread_name(format!("{}-reactor", cfg.name))
            .enable_all()
            .build()?;

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
            // Handle 0 is reserved, so an uninitialised field in a guest
            // names no socket.
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

        cell.me
            .set(Arc::downgrade(&cell))
            .expect("the self-reference is set once, here, on a fresh OnceLock");
        cell.start_exec_pool()?;
        cell.start_idle_sweeper()?;
        Ok(cell)
    }

    /// `None` only while the cell is being dropped, when a late dispatch is
    /// refused rather than served.
    pub(crate) fn me(&self) -> Option<Arc<Cell>> {
        self.me.get().and_then(Weak::upgrade)
    }

    fn start_exec_pool(self: &Arc<Self>) -> crate::Result<()> {
        let mut handles = Vec::new();
        for i in 0..self.cfg.exec_threads {
            let me = Arc::downgrade(self);
            let queue = self.queue.clone();
            let name = format!("{}-exec-{i}", self.cfg.name);
            handles.push(
                std::thread::Builder::new()
                    .name(name)
                    .spawn(move || Cell::exec_loop(me, queue, i))?,
            );
        }
        *self.workers.lock() = handles;
        Ok(())
    }

    fn start_idle_sweeper(self: &Arc<Self>) -> crate::Result<()> {
        let me = Arc::downgrade(self);
        let interval = Duration::from_millis(self.cfg.sweep_interval_ms.max(1));
        let stop = self.shutdown.clone();
        let name = format!("{}-idle", self.cfg.name);
        let h = std::thread::Builder::new().name(name).spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(interval);
                let Some(cell) = me.upgrade() else { return };
                // Nothing restarts this thread, and its loss is silent:
                // captures stop and every durable instance stays resident.
                let swept =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cell.sweep_idle()));
                if swept.is_err() {
                    cell.note_caught_panic("sweep");
                }
            }
        })?;
        self.workers.lock().push(h);
        Ok(())
    }

    /// One execution thread. The job body is caught: a host panic inside a
    /// dispatch costs that request, a counter and a log line rather than the
    /// thread. What it leaks is the instance, which stays in `residents` and
    /// in the gauges, and that dispatch's futures; `answer_after_panic`
    /// removes the `active` entry and answers the caller.
    fn exec_loop(me: Weak<Self>, queue: Arc<WorkQueue>, worker: usize) {
        while let Some(job) = queue.pop(worker) {
            let cell = me.upgrade();
            if let Some(cell) = &cell {
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
                    Job::Dispatch(j) => cell.run_dispatch(*j),
                    Job::Wake(w) => cell.run_wake(w.slot),
                }));
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

    /// Answers the caller of a panicked dispatch with a 500, if the request in
    /// `active` is still the one this thread was running; a mismatch means it
    /// was already cleaned up. The slot's lifecycle is left alone, because
    /// nothing here knows what state the panic left.
    fn answer_after_panic(&self, holding: Option<(InstanceId, u64)>) {
        let Some((id, dispatch_id)) = holding else {
            return;
        };
        // One acquisition for the check and the take: `(true, None)` is our
        // own entry already answered, which still needs removing.
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
            answer(
                &tx,
                DispatchResult {
                    status: 500,
                    body: b"host fault: a panic was caught inside this dispatch".to_vec(),
                    trap: Some("host_panic".into()),
                    instance: id,
                    ..DispatchResult::shed(dispatch_id)
                },
            );
        }
        self.active.remove(&id);
    }

    fn note_caught_panic(&self, stage: &'static str) {
        self.caught_panics.fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            cell = %self.cfg.name, stage,
            "a host panic was caught at the thread boundary; a guest fault is a trap, so this \
             is a host bug"
        );
    }

    /// Host panics caught and contained; not zero is a host bug. On the cell
    /// rather than [`Metrics`] because it is a fact about this build, not a
    /// tenant.
    pub fn caught_panics(&self) -> u64 {
        self.caught_panics.load(Ordering::Relaxed)
    }

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

    pub fn clear_breaker(&self, module: &ModuleId) {
        self.breaker.clear_module(module);
    }

    /// `(warm, pooled-fresh)`. An instance is Parked from the moment the guest
    /// returns but acquirable only once `finish_and_release` pools it, and the
    /// response leaves before that.
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
    pub fn reactor(&self) -> &tokio::runtime::Handle {
        &self.rt.reactor
    }

    /// Blocks until the response is final: a dispatch that parks and is later
    /// woken keeps this call outstanding.
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
            self.metrics.shed.fetch_add(1, Ordering::Relaxed);
            if let Some(m) = self.meters(module) {
                m.record_shed();
            }
            return Ok(DispatchResult::shed(dispatch_id));
        }
        rx.recv_timeout(timeout)
            .map_err(|_| anyhow::anyhow!("dispatch did not complete within {timeout:?}"))
    }

    /// Submits without waiting; `None` if the cell shed at admission.
    pub fn submit(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
    ) -> Option<std::sync::mpsc::Receiver<DispatchResult>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        self.submit_with(module, target, rec, tx).then_some(rx)
    }

    /// Submits onto a caller-provided channel that many submissions may
    /// share; results carry [`DispatchResult::dispatch_id`] for matching.
    /// Returns `false` if the cell shed at admission, and sends nothing in
    /// that case, so a caller draining the channel counts each shed once. A
    /// breaker rejection is sent, because it returns `true`. The sender is an
    /// execution-pool thread, so a channel the caller lets fill stops the
    /// cell.
    pub fn submit_with(
        &self,
        module: &ModuleId,
        target: Target,
        rec: DispatchRecord,
        responder: std::sync::mpsc::SyncSender<DispatchResult>,
    ) -> bool {
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
            answer(&responder, DispatchResult::breaker_open(dispatch_id));
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

    /// The completion path for an alarm, a socket frame, a DI reply or a
    /// test; the vtable's own futures come through the same runtime call.
    pub fn complete_future(&self, id: u64, result: wisp_abi::result::ResultRecord) {
        self.rt.settle_future(id, result);
    }

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

    fn run_dispatch(self: &Arc<Self>, job: DispatchJob) {
        let did = job.rec.dispatch_id;
        let entry = self.modules.read().get(&job.module).cloned();
        let Some(entry) = entry else {
            answer(
                &job.responder,
                DispatchResult {
                    status: 404,
                    body: b"no such module".to_vec(),
                    ..DispatchResult::shed(did)
                },
            );
            return;
        };

        let t0 = Instant::now();
        let acquired = match &job.target {
            Target::Handler { .. } => self.acquire_handler(&entry),
        };
        let (slot, src) = match acquired {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, module = %job.module, "acquisition failed");
                answer(
                    &job.responder,
                    DispatchResult {
                        status: 500,
                        body: e.to_string().into_bytes(),
                        ..DispatchResult::shed(did)
                    },
                );
                return;
            }
        };
        self.note_acquired(src, t0.elapsed());
        let acquired = src.label();

        let budget = CpuBudget {
            cpu_ms: job.rec.cpu_budget_ms,
            wall_ms_cap: self.cfg.wall_ms_cap,
            // Later slices carry what earlier ones spent, so the budget binds
            // the dispatch rather than each wake.
            spent_ns: 0,
        };
        // A Handler never reaches this: a parked instance is not in the pool.
        // A durable instance is addressed by identity and takes one dispatch
        // at a time; serialising its messages is the durability layer's job.
        // Counted before `dispatches`, so a 409 is not a dispatch.
        if self.active.contains_key(&slot.id) {
            self.metrics
                .single_writer_refusals
                .fetch_add(1, Ordering::Relaxed);
            answer(
                &job.responder,
                DispatchResult {
                    status: 409,
                    body: "durable instance already has a dispatch in flight"
                        .as_bytes()
                        .to_vec(),
                    ..DispatchResult::shed(did)
                },
            );
            return;
        }
        // The cancellation future is allocated before the guest is entered,
        // because the epoch callback that completes it runs with the guest on
        // the stack.
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

        IN_FLIGHT.set(Some((slot.id, job.rec.dispatch_id)));

        entry.meters.record_dispatch();
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);

        slot.ctx
            .begin_dispatch(crate::now_ms(), job.rec.dispatch_id);

        let outcome = {
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(e) => {
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
            if ends_in_tombstone(&out, &slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            out
        };

        // A completion that landed while the dispatch was Hot waited on the
        // reentrancy rule; hand the instance to the wake lane for it.
        if !self.settle(&slot, &entry, outcome) {
            self.queue_wake(slot);
        }
    }

    /// The metrics for how a dispatch got its instance.
    fn note_acquired(&self, src: AcquireSource, took: Duration) {
        match src {
            AcquireSource::WarmParked => &self.metrics.served_warm,
            AcquireSource::PooledFresh => &self.metrics.served_pooled_fresh,
            AcquireSource::HibernationWake => {
                self.metrics
                    .hibernation_wake_us
                    .record(took.as_micros() as u64);
                self.metrics
                    .hibernation_wakes
                    .fetch_add(1, Ordering::Relaxed);
                &self.metrics.served_hibernation_wake
            }
            AcquireSource::Activated => {
                self.metrics.activation_us.record(took.as_micros() as u64);
                &self.metrics.served_activated
            }
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    fn queue_wake(&self, slot: Arc<Slot>) {
        self.metrics.in_cell_wakes.fetch_add(1, Ordering::Relaxed);
        self.queue.push_wake(WakeItem { slot });
    }

    /// One woken slice, and every further slice the instance can take without
    /// going back through a lane.
    fn run_wake(self: &Arc<Self>, slot: Arc<Slot>) {
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
            // The instance is entered before the result is taken out, so a
            // result is never pulled with nowhere to deliver it. A busy
            // instance drains the result itself when its call returns.
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(e @ (EnterError::Busy | EnterError::Tombstoned)) => {
                    self.count_enter_refusal(e);
                    return;
                }
                Err(EnterError::NotResident(_)) => return,
            };
            let Some((fid, res)) = self.rt.futures.take_ready(slot.id) else {
                // The first wake's loop drained every completion.
                return;
            };
            // The budget binds the dispatch, so a wake resumes with what
            // earlier slices burned; otherwise a guest could park and wake
            // indefinitely on a fresh budget each time.
            let budget = self
                .active
                .with_mut(&slot.id, |a| a.budget.after(a.cpu_ns))
                .unwrap_or_else(CpuBudget::http);

            slot.ctx
                .begin_slice(crate::now_ms(), slot.ctx.dispatch_id());

            let t0 = Instant::now();
            self.hot.fetch_add(1, Ordering::Relaxed);
            let outcome = guard.instance().wake(fid, &res, budget);
            self.hot.fetch_sub(1, Ordering::Relaxed);
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

    /// Returns true when the instance takes no more wakes in this loop.
    fn settle(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        out: DispatchOutcome,
    ) -> bool {
        // The Hot interval, overage included: the meter reads the thread
        // clock and does not know what a budget is.
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

        // A latched trap becomes a trap here, before the guest proceeds as
        // though the import had succeeded. A clean kill is left alone: its
        // latched code is almost always a poisoned import the guest ignored
        // on its way out, and its slot is consistent.
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
            Completion::Parked => self.park_after_slice(slot),
            Completion::Done => {
                // An unfinished streaming response stays in the table for the
                // `resp_write`s that follow `done`. The caller receives the
                // bytes accumulated so far; later chunks are readable through
                // `Cell::response_snapshot` and are not piped to it.
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
                        slot.park(crate::now_ms());
                        !self.rt.futures.has_ready(slot.id)
                    }
                    RequestEnd::Disposed(d) => {
                        self.breaker.record_success(&breaker_key);
                        self.finish_and_release(slot, entry, d);
                        true
                    }
                    RequestEnd::Parked => self.park_after_slice(slot),
                }
            }
            // The request still fails with `budget_exceeded`, but the
            // instance is consistent, its slot is reused, and the breaker
            // records neither a kill nor a success: a clean kill is the budget
            // working as designed.
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
                        self.finish_and_release(slot, entry, d);
                        true
                    }
                    RequestEnd::Parked | RequestEnd::RetainedForWaitUntil => {
                        self.park_after_slice(slot)
                    }
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
                    self.breaker.record_kill(&breaker_key, crate::now_ms());
                } else {
                    entry.meters.record_trap();
                    // A hard kill is not a trap, or `tombstones` would not
                    // equal `hard_kills + traps`.
                    self.metrics.traps.fetch_add(1, Ordering::Relaxed);
                }
                slot.tombstone();
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
                self.finish_and_release(slot, entry, evict());
                true
            }
        }
    }

    /// The instance parks between slices. Returns whether the wake loop is done with it.
    fn park_after_slice(&self, slot: &Arc<Slot>) -> bool {
        slot.park(crate::now_ms());
        // A completion that landed while Hot is drained now.
        !self.rt.futures.has_ready(slot.id)
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
            answer(&tx, result);
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
            if let (Some(now), Some(started)) = (crate::mono_ns(), r.started_ns) {
                self.metrics
                    .request_us
                    .record(now.saturating_sub(started) / 1_000);
            }
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

    /// The books for a guest that trapped or was killed inside the pre-capture
    /// gc hint, on the sweeper's thread with no request to answer. No overage
    /// is claimed: the hint runs on a cap, not a budget the tenant bought.
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
        self.forget(slot);
        tracing::warn!(
            tenant = %entry.tenant, instance = %slot.id, ?completion,
            "the guest ended its pre-capture gc-hint in a tombstone; the instance is \
             discarded and the slot keeps refusing"
        );
    }

    fn remember_tombstone(&self, slot: &Arc<Slot>) {
        /// Covers the burst a breaker takes to notice.
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

    pub fn tombstoned_instances(&self) -> Vec<InstanceId> {
        self.tombstoned.lock().iter().map(|s| s.id).collect()
    }

    fn count_enter_refusal(&self, e: EnterError) {
        match e {
            EnterError::Busy => {
                self.metrics
                    .reentrancy_refusals
                    .fetch_add(1, Ordering::Relaxed);
            }
            EnterError::Tombstoned => {
                self.metrics
                    .tombstone_refusals
                    .fetch_add(1, Ordering::Relaxed);
            }
            // An instance that is not here, counted by the tier gauges.
            EnterError::NotResident(_) => {}
        }
    }

    /// A call into an instance through the same door every dispatch uses,
    /// with the refusal counted. A killed instance refuses as a tombstone
    /// rather than reading as "no such instance".
    pub fn with_instance<R>(
        &self,
        id: InstanceId,
        f: impl FnOnce(&mut (dyn wisp_engine::InstanceOps + Send)) -> R,
    ) -> Result<R, EnterError> {
        let Some(slot) = self.slot(id) else {
            return Err(EnterError::NotResident(LifecycleTier::Discarded));
        };
        // Bound, because the temporary borrows `slot`.
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

    fn make_ctx(&self, entry: &Arc<ModuleEntry>, id: InstanceId) -> Arc<InstanceCtx> {
        Arc::new(InstanceCtx::new(
            id,
            Tenancy {
                module: entry.activator.module().clone(),
                tenant: entry.tenant.clone(),
                limits: entry.limits,
                lifecycle: entry.manifest.handlers.lifecycle,
                allowlist: entry.manifest.bindings.fetch_allowlist.clone(),
            },
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

    /// Falls back to the tombstone ring, so a killed instance refuses rather
    /// than reading as "no such instance".
    pub fn slot(&self, id: InstanceId) -> Option<Arc<Slot>> {
        if let Some(s) = self.residents.lock().get(&id).map(|r| r.slot.clone()) {
            return Some(s);
        }
        self.tombstoned.lock().iter().find(|s| s.id == id).cloned()
    }

    pub fn module_tenant(&self, module: &ModuleId) -> Option<String> {
        self.modules.read().get(module).map(|m| m.tenant.clone())
    }

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
    /// The bytes a streaming response has accumulated so far.
    pub fn response_snapshot(&self, id: InstanceId, handle: u32) -> Option<Vec<u8>> {
        let g = self.residents.lock();
        let r = g.get(&id)?;
        let t = r.slot.ctx.responses.lock();
        t.get(handle).map(|resp| resp.body())
    }

    pub fn is_quarantined(&self, tenant: &str) -> bool {
        self.quarantined.lock().iter().any(|t| t == tenant)
    }

    /// Idempotent.
    pub fn shutdown(&self) {
        self.stop(false);
    }

    fn stop(&self, from_drop: bool) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.queue.close();
        let me = std::thread::current().id();
        for h in self.workers.lock().drain(..) {
            // The last strong reference can live on a pool thread, so `Drop`
            // runs there; joining yourself is a deadlock.
            if from_drop && h.thread().id() == me {
                continue;
            }
            if h.join().is_err() {
                tracing::error!(cell = %self.cfg.name, "a cell thread panicked on its way out");
            }
        }
    }
}

/// Sends a result to a caller that may have stopped waiting, which is not an
/// error in the cell.
fn answer(tx: &std::sync::mpsc::SyncSender<DispatchResult>, result: DispatchResult) {
    let dispatch_id = result.dispatch_id;
    if tx.send(result).is_err() {
        tracing::debug!(
            dispatch_id,
            "the caller stopped waiting before its dispatch was answered"
        );
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

/// Cell-level tests: what survives a host panic.
#[cfg(test)]
mod cell_tests {
    use super::*;
    use crate::fakes::*;
    use wisp_abi::config::{AppManifest, Bindings, HandlerConfig};

    /// Stands in for a host assertion firing under a guest call.
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
        cell.register_module("acme", act, manifest(name))
            .expect("register");
        module
    }

    fn cell(cfg: CellConfig) -> Arc<Cell> {
        Cell::new(cfg, services()).expect("a cell starts")
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

    /// One execution thread, so there is no second one to hide the loss. The
    /// panic's backtrace on stderr is intended: a caught host panic is a host
    /// bug and stays loud.
    #[test]
    fn a_panicking_dispatch_does_not_take_the_execution_thread_with_it() {
        let cell = cell(CellConfig {
            exec_threads: 1,
            ..CellConfig::for_test("panic-dispatch")
        });

        let bad = ModuleId::new("bad", "v1");
        cell.register_module(
            "acme",
            Arc::new(PanicActivator {
                module: bad.clone(),
                template: Arc::new(vec![0u8; wisp_abi::PAGE_SIZE]),
            }),
            manifest("bad"),
        )
        .expect("register");

        let good = app(&cell, "good", FakeBehaviour::done());

        let route = |p: &str| Target::Handler { route: p.into() };
        let failed = cell
            .dispatch_blocking(&bad, route("/go"), http(1), Duration::from_secs(10))
            .expect("the caller must be answered, not left waiting");
        assert_eq!(failed.status, 500, "a host fault is a 500: {failed:?}");
        assert_eq!(
            failed.trap.as_deref(),
            Some("host_panic"),
            "and it says what happened"
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
            "exactly one panic is counted; an uncounted contained fault is a silently slower \
             cell"
        );
        cell.shutdown();
    }

    /// Constructs the state the race produces, a live request on an instance
    /// and a handler holding a stale dispatch id for it, rather than racing
    /// threads. Both directions are asserted, or a handler that never answers
    /// would satisfy the guard.
    #[test]
    fn a_stale_panic_handler_does_not_answer_another_requests_caller() {
        let cell = cell(CellConfig {
            exec_threads: 2,
            ..CellConfig::for_test("stale-panic")
        });
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

        cell.answer_after_panic(Some((id, LIVE)));
        let out = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the owning handler must answer");
        assert_eq!(out.status, 500, "a host fault is a 500");
        assert_eq!(
            out.trap.as_deref(),
            Some("host_panic"),
            "and names the panic"
        );
        assert_eq!(out.dispatch_id, LIVE, "for the live dispatch");
        cell.shutdown();
    }
}
