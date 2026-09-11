//! The sandbox boundary: everything that touches wasmtime. Templates are
//! pre-initialised memory images mapped copy-on-write by wasmtime's pooling
//! allocator; an activation is a `Store` plus `Linker::instantiate` into a
//! pre-reserved slot, and a reset drops the store and instantiates again. A
//! dispatch is a synchronous call on the caller's thread, metered by the
//! thread CPU clock and killed through the epoch counter.

use std::sync::Arc;

use wasmtime::Linker;

pub mod bindings;
pub mod budget;
pub mod image;
pub mod instance;
pub mod pal;
pub mod snapshot;
pub mod template;

mod activate;
mod artifact;
mod call;
mod capture;
mod layout;
mod linker;
mod meter;
mod state;
mod watchdog;
mod wtconfig;

pub use artifact::ArtifactPaths;
pub use bindings::{HostBindings, ImportCall};
pub use budget::{
    CpuBudget, CpuMeasurement, DispatchOutcome, ExecutionLimit, KillPhase, TrapInfo, TrapKind,
};
pub use image::{MemoryImage, PAGE};
pub use instance::{Completion, Instance, InstanceOps};
pub use meter::clock_read_failures;
pub use pal::platform_files::{PlatformFile, PlatformFiles, PlatformFilesError};
pub use snapshot::InstanceImage;
pub use template::{ModuleHash, Template, DEFERRED_INIT_SECTION};

use state::StoreState;

pub type Result<T> = anyhow::Result<T>;

/// Engine-wide configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Ceiling on concurrently resident instances in this cell.
    pub max_instances: u32,
    /// Per-instance linear-memory ceiling.
    pub max_memory_bytes: u64,
    /// Period of the epoch watchdog. Grace is delivered by epoch ticks, so
    /// [`WispEngine::new`] refuses a tick too coarse to fit
    /// [`budget::MIN_GRACE_TICKS`] inside [`budget::GRACE_FLOOR_NS`].
    pub epoch_tick_us: u64,
    pub pooling: bool,
    /// Cranelift bounds-check masking and pointer sanitisation on loads.
    pub spectre_mitigations: bool,
    /// `PROT_NONE` space after each slot's linear memory, never mapped, so
    /// Cranelift can drop the bounds check on any access whose static offset
    /// lands inside it. It does not enlarge what a guest may address. The
    /// pooling allocator reserves `max_instances * (max_memory_bytes + this)`
    /// of address space up front, so at 2 GiB a cell could not be configured
    /// past about ten thousand instances on x86-64; 32 MiB is wasmtime's own
    /// default and leaves room for the densities this platform is built for.
    /// Changing it changes generated code and so invalidates every cached
    /// artifact through the engine fingerprint.
    pub guard_size_bytes: u64,
    /// Read-only files the PAL filesystem serves to every guest, at
    /// pre-initialisation and at dispatch alike. Empty by default.
    pub platform_files: PlatformFiles,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            max_instances: 10_000,
            max_memory_bytes: 128 << 20,
            epoch_tick_us: 200,
            pooling: true,
            spectre_mitigations: true,
            guard_size_bytes: 32 << 20,
            platform_files: PlatformFiles::default(),
        }
    }
}

/// A configured wasmtime engine plus its epoch watchdog.
pub struct WispEngine {
    inner: Arc<EngineInner>,
}

pub(crate) struct EngineInner {
    pub(crate) wasmtime: wasmtime::Engine,
    pub(crate) cfg: EngineConfig,
    /// Built once; a `Linker<T>` is reusable across every store of the same
    /// `T`, and the per-instance context lives in the store data.
    pub(crate) linker: Linker<StoreState>,
    pub(crate) watchdog: watchdog::Watchdog,
    pub(crate) fingerprint: String,
}

impl WispEngine {
    /// Fails on a configuration wasmtime rejects, on an epoch tick too coarse
    /// to deliver the grace window, and on an engine that turns out to accept
    /// threads or shared memory.
    pub fn new(cfg: EngineConfig) -> Result<Self> {
        budget::check_grace_deliverable(cfg.epoch_tick_us)?;
        let wasmtime = wtconfig::build_engine(&cfg)?;
        wtconfig::assert_feature_set(&wasmtime)?;
        let linker = linker::build_linker(&wasmtime).map_err(anyhow::Error::from)?;
        let watchdog = watchdog::Watchdog::start(wasmtime.clone(), cfg.epoch_tick_us)?;
        let fingerprint = wtconfig::engine_fingerprint(&wasmtime, &cfg);
        Ok(Self {
            inner: Arc::new(EngineInner {
                wasmtime,
                cfg,
                linker,
                watchdog,
                fingerprint,
            }),
        })
    }
}
