//! Local portable initialization through the worker execution engine.

use crate::{BuildRequest, Initialization, PrepareError, RuntimeProfile};
use isoplane_abi::{
    config::{GuestMemoryBytes, InitSandbox, Limits},
    imports,
};
use isoplane_engine::{EngineConfig, IsoplaneEngine, ModuleHash, PlatformFiles};

/// Default local initialization occupancy budget across replay passes.
pub const DEFAULT_INITIALIZATION_OCCUPANCY_MS: u32 = 30_000;
/// Default local initialization wall backstop, including waits and replay passes.
pub const DEFAULT_INITIALIZATION_WALL_MS: u32 = 60_000;
/// Default local initialization replay limit.
pub const DEFAULT_INITIALIZATION_PASSES: u32 = 16;
/// Default retained initialization log bytes.
pub const DEFAULT_INITIALIZATION_LOG_BYTES: usize = 64 * 1024;

/// Configures local initialization; hosted callers select an isolated Preparation provider.
#[derive(Default)]
pub struct PreInitializer {
    pal: InitSandbox,
    runtime: RuntimeProfile,
    platform_files: PlatformFiles,
    memory_limit: Option<GuestMemoryBytes>,
    engine: EngineConfig,
    runtime_config: Option<Vec<u8>>,
    network: Option<crate::InitializationNetwork>,
}

/// Immutable inputs to one local template build.
#[derive(Debug, Clone)]
pub struct PreInitRequest {
    /// Digest of the exact source module.
    pub module_hash: ModuleHash,
    /// Serialized configuration available to initialization.
    pub init_blob: Vec<u8>,
}

/// Portable template and initialization observations.
#[derive(Debug, Clone)]
pub struct PreInitOutcome {
    /// The module with its data segments replaced by the post-init image and
    /// the init export removed.
    pub template_wasm: Vec<u8>,
    /// Init units left un-run; they run per instance on first touch.
    pub deferred_units: Vec<u32>,
    /// Number of initialization passes executed.
    pub passes: u32,
    /// Preparation-worker occupancy, including compilation, in milliseconds.
    pub init_occupancy_ms: u64,
    /// Length of the portable initialized module in bytes.
    pub template_bytes: u64,
    /// Digest of explicit initialization inputs; fresh random output is excluded.
    pub reproducible_digest: [u8; 32],
    /// The capability each deferred unit reached for.
    pub deferred_reasons: Vec<(u32, String)>,
    /// `log_emit` output from the final pass.
    pub init_logs: Vec<String>,
    /// Capability refusals over all passes.
    pub capability_refusals: u32,
    /// Runtime-PAL imports the guest reached outside any init unit, with
    /// counts.
    pub pal_calls: Vec<(String, u32)>,
    /// Hash-seed reads on the successful initialization pass.
    pub seed_reads: u32,
}

/// Refusals from local preparation.
#[derive(Debug, thiserror::Error)]
pub enum PreInitError {
    /// Native compiler workers could not be created.
    #[error("creating pre-initialisation compiler workers: {0}")]
    CompilerWorkers(#[from] rayon::ThreadPoolBuildError),
    /// The source has no initialization function.
    #[error("module does not export `{0}`; nothing to pre-initialize")]
    NoInitFunc(&'static str),
    /// Nothing to defer: deferring a guess would change program behaviour.
    #[error(
        "pre-init trapped outside any init unit (`{import}`); wrap initialization that may \
         touch host capabilities in an init unit: {cause}"
    )]
    Unattributable {
        import: String,
        cause: anyhow::Error,
    },
    /// A deferred unit ignored the replay decision.
    #[error(
        "init unit {unit} trapped on `{import}` again after being deferred; the guest is \
         ignoring `init_unit_deferred`"
    )]
    DeferralIgnored { unit: u32, import: String },
    /// Initialization exceeded its replay limit.
    #[error("pre-init did not converge in {0} passes")]
    NoConvergence(u32),
    /// The declared memory exceeds the selected initialization capacity.
    #[error("declared init memory of {bytes} bytes exceeds the init limit of {limit} bytes")]
    MemoryLimit { bytes: u64, limit: u64 },
    /// A guest `abort`, an `unreachable`, an out-of-memory: not a sealed
    /// capability, so nothing to defer.
    #[error("pre-init trapped: {0}")]
    Trapped(anyhow::Error),
}

impl PreInitializer {
    /// Select sealed initialization with no platform files.
    pub fn new() -> Self {
        Self::default()
    }
    /// Use the consuming engine's allocation and compiler settings.
    pub fn with_engine_config(mut self, engine: EngineConfig) -> Self {
        self.engine = engine;
        self
    }
    /// Select the trusted producer's permitted module declarations.
    pub fn with_runtime_profile(mut self, runtime: RuntimeProfile) -> Self {
        self.runtime = runtime;
        self
    }
    /// Match initialization to the consuming engine's linear-memory capacity.
    pub fn with_memory_limit(mut self, memory_limit: GuestMemoryBytes) -> Self {
        self.memory_limit = Some(memory_limit);
        self
    }
    /// Select the caller's initialization capability policy.
    pub fn with_pal(mut self, pal: InitSandbox) -> Self {
        self.pal = pal;
        self
    }
    /// Serve the same read-only platform files supplied during execution.
    pub fn with_platform_files(mut self, files: PlatformFiles) -> Self {
        self.platform_files = files;
        self
    }
    /// Supplies the deployment settings used during local initialization.
    /// Use the application's outbound policy during asynchronous initialization.
    pub fn with_network(mut self, network: crate::InitializationNetwork) -> Self {
        self.network = Some(network);
        self
    }

    pub fn with_runtime_configuration(mut self, configuration: Vec<u8>) -> Self {
        self.runtime_config = Some(configuration);
        self
    }
    /// Initialize locally using the shared execution engine and portable state baker.
    pub fn run(&self, wasm: &[u8], req: &PreInitRequest) -> Result<PreInitOutcome, PreInitError> {
        if req.module_hash != ModuleHash::of(wasm) {
            return Err(PreInitError::Trapped(anyhow::anyhow!(
                "initialization source digest mismatch"
            )));
        }
        let (has_initializer, bytes) = module_shape(wasm).map_err(PreInitError::Trapped)?;
        if !has_initializer {
            return Err(PreInitError::NoInitFunc(imports::exports::INITIALIZE));
        }
        let limit = self.memory_limit.map_or(536870912, GuestMemoryBytes::get);
        let declaration_limit = self
            .runtime
            .declared_memory_ceiling(self.memory_limit)
            .map_or(limit, GuestMemoryBytes::get);
        if bytes > declaration_limit {
            return Err(PreInitError::MemoryLimit {
                bytes,
                limit: declaration_limit,
            });
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .stack_size(self.engine.compiler_stack_bytes)
            .build()?;
        pool.install(|| {
            let configuration = EngineConfig {
                pooling: false,
                // Initialization runs within the limit itself; the ceiling past
                // it is for dispatches.
                max_memory_bytes: limit,
                platform_files: self.platform_files.clone(),
                ..self.engine.clone()
            };
            let engine =
                IsoplaneEngine::new(configuration.clone()).map_err(PreInitError::Trapped)?;
            let request = Initialization {
                network: self.network.clone(),
                config: req.init_blob.clone(),
                runtime_config: self
                    .runtime_config
                    .clone()
                    .unwrap_or_else(|| b"{}".to_vec()),
                sandbox: self.pal,
                occupancy_ms: DEFAULT_INITIALIZATION_OCCUPANCY_MS,
                wall_ms: DEFAULT_INITIALIZATION_WALL_MS,
                max_passes: DEFAULT_INITIALIZATION_PASSES,
                log_bytes: DEFAULT_INITIALIZATION_LOG_BYTES,
            };
            let crate::InitializedModule {
                wasm: template_wasm,
                initialized: output,
            } = crate::initialize(&engine, wasm, &request).map_err(map_error)?;
            let digest = BuildRequest {
                guest_memory_bytes: self.memory_limit,
                validation_limits: Default::default(),
                engine: configuration,
                application_files: PlatformFiles::default(),
                runtime: self.runtime.clone(),
                limits: Limits {
                    instances_per_application: 16,
                    module_bytes_compressed: 52428800,
                    package_bytes: 52428800,
                    instance_memory_max: 536870912,
                    occupancy_ms_per_dispatch: 300000,
                    subrequests_per_dispatch: 10000,
                    pending_futures_per_dispatch: 4096,
                },
                initialization: request,
                input_hash: req.module_hash.0,
                portable_template: crate::PortableTemplate::Returned,
            }
            .identity()
            .map_err(|error| PreInitError::Trapped(error.into()))?
            .initialization_hash;
            Ok(PreInitOutcome {
                template_bytes: template_wasm.len() as u64,
                template_wasm,
                deferred_units: output.deferred,
                passes: output.passes,
                init_occupancy_ms: output.preparation_occupancy_ns / 1_000_000,
                reproducible_digest: digest,
                deferred_reasons: output.deferred_reasons,
                init_logs: output.logs,
                capability_refusals: output.capability_refusals,
                pal_calls: output.pal_calls,
                seed_reads: output.seed_reads,
            })
        })
    }
}

fn map_error(error: PrepareError) -> PreInitError {
    match error {
        PrepareError::Unattributable { import, detail, .. } => PreInitError::Unattributable {
            import,
            cause: anyhow::anyhow!(detail),
        },
        PrepareError::DeferralIgnored { unit, import } => {
            PreInitError::DeferralIgnored { unit, import }
        }
        PrepareError::NoConvergence(passes) => PreInitError::NoConvergence(passes),
        // Other preparation failures have no more specific local replay classification.
        error => PreInitError::Trapped(error.into()),
    }
}

fn module_shape(wasm: &[u8]) -> anyhow::Result<(bool, u64)> {
    let mut has_initializer = false;
    let mut memory_bytes = 0u64;
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload? {
            wasmparser::Payload::ExportSection(reader) => {
                for export in reader {
                    let export = export?;
                    has_initializer |= export.name == imports::exports::INITIALIZE
                        && export.kind == wasmparser::ExternalKind::Func;
                }
            }
            wasmparser::Payload::MemorySection(reader) => {
                for memory in reader {
                    let memory = memory?;
                    memory_bytes = memory_bytes.saturating_add(
                        memory
                            .maximum
                            .unwrap_or(memory.initial)
                            .saturating_mul(isoplane_abi::WASM_PAGE_SIZE as u64),
                    );
                }
            }
            _ => {}
        }
    }
    Ok((has_initializer, memory_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_module(memory: &str, initialize: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module
            (memory (export "memory") {memory})
            (func (export "isoplane_initialize") (result i64) {initialize} i64.const 0)
            (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 0))
            (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 1))
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 1)))"#
        ))
        .unwrap()
    }

    fn request(wasm: &[u8]) -> PreInitRequest {
        PreInitRequest {
            module_hash: ModuleHash::of(wasm),
            init_blob: vec![],
        }
    }

    #[test]
    fn local_initialization_accepts_the_explicit_engine_capacity_above_its_default() {
        let wasm = memory_module("1 16384", "");
        assert!(
            matches!(
                PreInitializer::new().run(&wasm, &request(&wasm)),
                Err(PreInitError::MemoryLimit { .. })
            ),
            "the existing default must retain its initialization memory ceiling"
        );
        let output = PreInitializer::new()
            .with_memory_limit(GuestMemoryBytes::new(1 << 30).unwrap())
            .run(&wasm, &request(&wasm))
            .unwrap();
        assert_eq!(
            output.passes, 1,
            "a configured larger engine must initialize a bounded module above the default ceiling"
        );
    }

    #[test]
    fn local_initialization_enforces_the_consuming_engine_capacity_during_growth() {
        let wasm = memory_module(
            "1 65536",
            "(if (i32.ne (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable))",
        );
        let output = PreInitializer::new()
            .with_runtime_profile(RuntimeProfile::SharedRuntime {
                sdk_version: "test".into(),
            })
            .with_memory_limit(GuestMemoryBytes::new(65536).unwrap())
            .run(&wasm, &request(&wasm))
            .unwrap();
        assert_eq!(
            output.passes, 1,
            "initialization must deny growth beyond its configured engine capacity"
        );
    }

    #[test]
    fn initialization_refuses_a_request_bound_to_another_source() {
        let wasm = wat::parse_str(
            "(module (func (export \"isoplane_initialize\") (result i64) i64.const 0))",
        )
        .expect("valid module");
        let request = PreInitRequest {
            module_hash: ModuleHash::of(b"another source"),
            init_blob: vec![],
        };
        assert!(
            PreInitializer::new().run(&wasm, &request).is_err(),
            "a template request must identify its exact source"
        );
    }
}
