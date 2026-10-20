//! The cell: reactor threads that own all I/O, an execution pool that runs
//! Hot dispatches as synchronous calls into the engine, the instance
//! registries, and the idle trajectory. There is no scheduler: the kernel
//! preempts a CPU-bound tenant on its own pool thread, and the cell controls
//! admission with a bounded lane per worker.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use isoplane_abi::config::{AppManifest, Limits};
use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, TraceContext};
use isoplane_abi::status::{PollStatus, TrapCode};
use isoplane_engine::{Completion, DispatchOutcome, GraceEnd, OccupancyBudget};

use crate::bindings::Tenancy;
use crate::bindings::{CellRuntime, InstanceCtx, WakeSink};
pub use crate::bindings::{Fetcher, Services};
use crate::breaker::{BreakerConfig, BreakerKey, HangBreaker};
use crate::disposition::{decide, evict, warm, DispatchEnd};
use crate::futures::FutureTable;
use crate::hibernate_policy::{self, HibernateConfig};
use crate::meters::Meters;
use crate::metrics::{Metrics, MetricsSnapshot};
use crate::pool::{AcquireSource, Activator, InstancePool};
use crate::sharded_map::ShardedMap;
use crate::slot::{EnterError, LifecycleTier, Slot};

mod background;
mod completion;
mod dispatch_api;
mod hibernate;
mod lifecycle;
mod pool;
mod queue;
pub(crate) use queue::HttpDispatch;
mod reclamation;
mod registry;
pub use reclamation::{ReclaimCandidate, ReclaimKind};
mod recovery;
mod timer_wakes;
mod wake;

pub use dispatch_api::{DispatchResult, Target};

use dispatch_api::{answer, ends_the_instance};

use crate::{InstanceId, ModuleId};
use completion::{DispatchCompletion, TerminalReason};
use pool::IN_FLIGHT;
use queue::{Delivery, DispatchJob, Job, WakeItem, WorkQueue};
use registry::{ActiveDispatch, ModuleEntry, Resident};
use wake::CellWake;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CellConfig {
    #[serde(skip)]
    pub name: String,
    /// The bound on concurrently Hot dispatches.
    pub exec_threads: usize,
    /// Async workers serving guest I/O and wake sources.
    pub reactor_threads: usize,
    /// Dispatches one worker's lane holds before the cell sheds. Each
    /// execution thread has its own lane, so the cell as a whole holds
    /// `exec_threads` times this.
    pub queue_depth_per_worker: usize,
    pub max_idle_per_module: usize,
    /// Per-instance byte cap for undelivered future payloads, resident or hibernated.
    pub buffered_future_bytes: usize,
    /// High-water mark for queued HTTP response bytes; writes wait for readers.
    /// This does not limit the total size of a response.
    #[serde(skip)]
    pub response_queue_bytes: usize,
    /// Wall-clock backstop for one slice; reported separately from active execution time.
    pub slice_wall_ms_cap: u32,
    /// How long a dispatch has to finish once its cancellation is requested,
    /// by its CPU budget or its client going away, before the host kills it,
    /// evicting its instance and failing every dispatch on it.
    pub cancellation_grace_ms: u64,
    /// Host resources retained on behalf of each guest instance.
    pub guest_resources: crate::bindings::GuestResourceLimits,
    pub hibernate: HibernateConfig,
    pub breaker: BreakerConfig,
    pub sweep_interval_ms: u64,
    /// How long an instance retaining background work or connections may go
    /// without a request before it is stopped.
    pub background_idle_limit_ms: u64,
}

impl Default for CellConfig {
    fn default() -> Self {
        let threads = crate::threads::ThreadAllocation::production();
        Self {
            name: "cell-0".into(),
            exec_threads: threads.execution,
            reactor_threads: threads.reactor,
            queue_depth_per_worker: 64,
            max_idle_per_module: 64,
            buffered_future_bytes: 256 * 1024,
            response_queue_bytes: isoplane_abi::host::DEFAULT_RESPONSE_QUEUE_BYTES,
            slice_wall_ms_cap: 30_000,
            cancellation_grace_ms: 5_000,
            guest_resources: Default::default(),
            hibernate: HibernateConfig::default(),
            breaker: BreakerConfig::default(),
            sweep_interval_ms: 250,
            background_idle_limit_ms: 5 * 60 * 1000,
        }
    }
}

impl CellConfig {
    /// Local development and full-host tests use the local CPU allocation.
    pub fn local() -> Self {
        let threads = crate::threads::ThreadAllocation::local();
        Self {
            exec_threads: threads.execution,
            reactor_threads: threads.reactor,
            ..Self::default()
        }
    }

    /// Validates settings for a cell driven by its own execution workers.
    pub fn validate(&self) -> crate::Result<()> {
        self.hibernate.validate()?;
        anyhow::ensure!(self.exec_threads > 0, "exec_threads must be positive");
        anyhow::ensure!(self.reactor_threads > 0, "reactor_threads must be positive");
        anyhow::ensure!(
            self.queue_depth_per_worker > 0,
            "queue_depth_per_worker must be positive"
        );
        anyhow::ensure!(
            self.sweep_interval_ms > 0,
            "sweep_interval_ms must be positive"
        );
        anyhow::ensure!(
            self.background_idle_limit_ms > 0,
            "background_idle_limit_ms must be positive"
        );
        anyhow::ensure!(
            self.cancellation_grace_ms > 0,
            "cancellation_grace_ms must be positive"
        );
        Ok(())
    }

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

/// How often the sweeper re-warms the page cache of templates in use. Warming
/// a resident 76 MB template file took about 55 us, against 5.5 ms to read it,
/// so this costs next to nothing, and catches an eviction within the interval.
const TEMPLATE_WARM_INTERVAL_MS: u64 = 10_000;

type StorageReclaimer = dyn Fn() -> crate::Result<bool> + Send + Sync;

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
    modules: RwLock<HashMap<ModuleId, Arc<ModuleEntry>>>,
    applications:
        Mutex<HashMap<registry::ApplicationKey, Weak<crate::admission::ApplicationInstances>>>,
    residents: Mutex<HashMap<InstanceId, Resident>>,
    active: ShardedMap<InstanceId, registry::ActiveDispatches>,
    /// Dispatches that joined an instance while another dispatch's slice ran,
    /// waiting to begin between slices.
    /// Dispatches that joined an instance while a slice ran, with when they
    /// began waiting for its thread.
    starts: ShardedMap<InstanceId, VecDeque<(Instant, DispatchJob)>>,
    completion_clock: RwLock<completion::CompletionClock>,
    snapshots: Arc<dyn isoplane_hibernate::snapshot::SnapshotStorage>,
    memory_pressure: RwLock<Arc<dyn crate::memory_pressure::MemoryPressureSource>>,
    reclaiming: AtomicBool,
    /// When the sweeper last re-warmed the templates in use.
    templates_warmed_ms: AtomicU64,
    node_coordinated_reclamation: AtomicBool,
    storage_reclaimer: RwLock<Option<Arc<StorageReclaimer>>>,
    breaker: HangBreaker,
    metrics: Arc<Metrics>,
    hot: AtomicUsize,
    next_dispatch: AtomicU64,
    shutdown: Arc<AtomicBool>,
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    caught_panics: AtomicU64,
}

impl Cell {
    /// Fails if the reactor or a pool thread cannot be started.
    pub fn new(cfg: CellConfig, services: Arc<Services>) -> crate::Result<Arc<Self>> {
        Self::with_snapshot_storage(
            cfg,
            services,
            Arc::new(isoplane_hibernate::snapshot::LocalSnapshotStorage::new(
                std::env::temp_dir(),
            )?),
        )
    }

    /// Constructs a cell whose local snapshots consume the supplied storage reservations.
    pub fn with_snapshot_storage(
        cfg: CellConfig,
        services: Arc<Services>,
        snapshots: Arc<dyn isoplane_hibernate::snapshot::SnapshotStorage>,
    ) -> crate::Result<Arc<Self>> {
        let cell = Self::build(cfg, services, snapshots)?;
        cell.start_exec_pool()?;
        cell.start_idle_sweeper()?;
        Ok(cell)
    }

    fn build(
        cfg: CellConfig,
        services: Arc<Services>,
        snapshots: Arc<dyn isoplane_hibernate::snapshot::SnapshotStorage>,
    ) -> crate::Result<Arc<Self>> {
        cfg.hibernate.validate()?;
        anyhow::ensure!(
            cfg.response_queue_bytes > 0,
            "response_queue_bytes must be positive"
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(cfg.reactor_threads)
            .thread_name(format!("{}-reactor", cfg.name))
            .enable_all()
            .build()?;

        let metrics = Arc::new(Metrics::new());
        let queue = Arc::new(WorkQueue::new(cfg.queue_depth_per_worker, cfg.exec_threads));
        let index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let futures = Arc::new(FutureTable::new(cfg.buffered_future_bytes));

        let rt = Arc::new(CellRuntime {
            runtime_configuration: Default::default(),
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
            completion_clock: RwLock::new(Arc::new(crate::mono_ns)),
            breaker: HangBreaker::new(cfg.breaker),
            rt,
            runtime: Some(runtime),
            queue,
            index,
            modules: RwLock::new(HashMap::new()),
            applications: Mutex::new(HashMap::new()),
            residents: Mutex::new(HashMap::new()),
            active: ShardedMap::new(ACTIVE_SHARDS),
            starts: ShardedMap::new(ACTIVE_SHARDS),
            snapshots,
            memory_pressure: RwLock::new(Arc::new(crate::memory_pressure::SystemMemoryPressure)),
            reclaiming: AtomicBool::new(false),
            templates_warmed_ms: AtomicU64::new(0),
            node_coordinated_reclamation: AtomicBool::new(false),
            storage_reclaimer: RwLock::new(None),
            metrics,
            hot: AtomicUsize::new(0),
            next_dispatch: AtomicU64::new(1),
            shutdown: Arc::new(AtomicBool::new(false)),
            workers: Mutex::new(Vec::new()),
            caught_panics: AtomicU64::new(0),
            cfg,
        });

        cell.me
            .set(Arc::downgrade(&cell))
            .expect("the self-reference is set once, here, on a fresh OnceLock");
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

    /// The live counters rather than a snapshot.
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
            let idle = m.pool.idle() as u64;
            parked += idle;
            live += idle;
        }
        self.metrics.live.store(live, Ordering::Relaxed);
        self.metrics.parked.store(parked, Ordering::Relaxed);
        self.metrics.hibernated.store(hib, Ordering::Relaxed);
        self.metrics.hot.store(hot, Ordering::Relaxed);
    }

    pub fn clear_breaker(&self, module: &ModuleId) {
        self.breaker.clear_module(module);
    }

    /// Warm instances waiting for a dispatch. An instance is Parked from the
    /// moment the guest returns but acquirable only once `finish_and_release`
    /// pools it, and the response leaves before that.
    pub fn idle_instances(&self, module: &ModuleId) -> Option<usize> {
        self.modules.read().get(module).map(|e| e.pool.idle())
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
