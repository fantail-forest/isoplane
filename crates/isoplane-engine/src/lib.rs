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
pub mod prepared;
pub mod snapshot;
pub mod template;

mod activate;
mod artifact;
mod call;
mod capture;
mod compiler;
mod layout;
mod linker;
mod meter;
mod state;
mod watchdog;
mod wtconfig;

pub use artifact::{ArtifactPaths, CompilationCache};
pub use bindings::{HostBindings, ImportCall};
pub use budget::{
    CpuBudget, CpuMeasurement, DispatchOutcome, ExecutionLimit, KillPhase, TrapInfo, TrapKind,
};
pub use compiler::COMPILER_STACK_BYTES;
pub use image::{MemoryImage, PAGE};
pub use instance::{Completion, Instance, InstanceOps};
pub use meter::clock_read_failures;
pub use meter::try_thread_cpu_nanos;
pub use pal::platform_files::{PlatformFile, PlatformFiles, PlatformFilesError};
pub use snapshot::InstanceImage;
pub use template::{ModuleHash, Template, DEFERRED_INIT_SECTION};

use state::StoreState;

pub type Result<T> = anyhow::Result<T>;

/// Engine-wide configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    /// Emit Linux perf jitdump records in this process's working directory.
    /// Disabled by default and excluded from serialized preparation settings.
    #[serde(skip)]
    pub perf_jitdump: bool,
    /// Ceiling on concurrently resident instances in this cell.
    pub max_instances: u32,
    /// Aggregate pooled table slots; omission follows the instance count.
    pub total_tables: Option<u32>,
    /// Per-instance linear-memory ceiling.
    pub max_memory_bytes: u64,
    /// Maximum metadata bytes retained by one pooled core instance.
    pub max_core_instance_size_bytes: usize,
    /// Maximum elements in each guest function table.
    pub table_elements: usize,
    /// Maximum function tables declared by one guest module.
    pub max_tables_per_module: u32,
    /// Native stack bytes reserved per Cranelift compiler worker.
    pub compiler_stack_bytes: usize,
    /// Maximum native stack bytes consumed by a guest invocation.
    pub max_wasm_stack_bytes: usize,
    /// Period of the epoch watchdog. Grace is delivered by epoch ticks, so
    /// [`IsoplaneEngine::new`] refuses a tick too coarse to fit
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
            perf_jitdump: false,
            max_instances: 10_000,
            total_tables: None,
            max_memory_bytes: 128 << 20,
            max_core_instance_size_bytes: isoplane_abi::config::DEFAULT_CORE_INSTANCE_SIZE_BYTES,
            table_elements: isoplane_abi::config::DEFAULT_TABLE_ELEMENTS,
            max_tables_per_module: isoplane_abi::config::DEFAULT_TABLES_PER_MODULE,
            compiler_stack_bytes: isoplane_abi::config::DEFAULT_COMPILER_STACK_BYTES,
            max_wasm_stack_bytes: isoplane_abi::config::DEFAULT_WASM_STACK_BYTES,
            epoch_tick_us: 200,
            pooling: true,
            spectre_mitigations: true,
            guard_size_bytes: 32 << 20,
            platform_files: PlatformFiles::default(),
        }
    }
}

/// A configured wasmtime engine plus its epoch watchdog.
pub struct IsoplaneEngine {
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

impl IsoplaneEngine {
    /// Settings used by this engine, for compatible external preparation.
    pub fn configuration(&self) -> EngineConfig {
        self.inner.cfg.clone()
    }

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
