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
use crate::disposition::{decide, evict, RequestEnd};
use crate::futures::FutureTable;
use crate::meters::Meters;
use crate::metrics::{Metrics, MetricsSnapshot};
use crate::pool::{AcquireSource, Activator, InstancePool};
use crate::sharded_map::ShardedMap;
use crate::slot::{EnterError, LifecycleTier, Slot};

mod dispatch_api;
mod hibernate;
mod lifecycle;
mod pool;
mod queue;
mod registry;
mod tombstone;
mod wake;

pub use dispatch_api::{DispatchResult, Target};

use dispatch_api::{answer, ends_in_tombstone};

use crate::{InstanceId, ModuleId};
use pool::IN_FLIGHT;
use queue::{DispatchJob, Job, WakeItem, WorkQueue};
use registry::{ActiveRequest, ModuleEntry, Resident};
use wake::CellWake;

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

/// Shards for `Cell::active`, above the default pool size so two threads are
/// not guaranteed to share one.
const ACTIVE_SHARDS: usize = 256;

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

    /// `(warm, awaiting reset)`. An instance is Parked from the moment the guest
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

    pub(crate) fn rt_services(&self) -> &Arc<Services> {
        &self.rt.services
    }

    pub(crate) fn next_dispatch_id(&self) -> u64 {
        self.next_dispatch.fetch_add(1, Ordering::Relaxed)
    }
}

impl Drop for Cell {
    fn drop(&mut self) {
        self.stop(true);
        if let Some(rt) = self.runtime.take() {
            // The last cell reference can be released by an async callback.
            rt.shutdown_background();
        }
    }
}
