//! Local portable initialization through the worker execution engine.

use crate::{BuildRequest, Initialization, PrepareError, RuntimeProfile};
use wisp_abi::{
    config::{InitSandbox, Limits},
    imports,
};
use wisp_engine::{EngineConfig, ModuleHash, PlatformFiles, WispEngine};

/// Default local initialization CPU budget across replay passes.
pub const DEFAULT_INITIALIZATION_CPU_MS: u32 = 30_000;
/// Default local initialization wall backstop per invocation.
pub const DEFAULT_INITIALIZATION_WALL_MS: u32 = 60_000;
/// Default local initialization replay limit.
pub const DEFAULT_INITIALIZATION_PASSES: u32 = 16;
/// Default retained initialization log bytes.
pub const DEFAULT_INITIALIZATION_LOG_BYTES: usize = 64 * 1024;

/// Configures local initialization; hosted callers select an isolated Preparation provider.
#[derive(Default)]
pub struct PreInitializer {
    pal: InitSandbox,
    platform_files: PlatformFiles,
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
    /// CPU of the preparation thread, including compilation, in milliseconds.
    pub init_cpu_ms: u64,
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
    /// The declared memory exceeds the initialization memory ceiling.
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
        let limit = 536870912;
        if bytes > limit {
            return Err(PreInitError::MemoryLimit { bytes, limit });
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .stack_size(wisp_engine::COMPILER_STACK_BYTES)
            .build()?;
        pool.install(|| {
            let configuration = EngineConfig {
                pooling: false,
                max_memory_bytes: limit,
                platform_files: self.platform_files.clone(),
                ..EngineConfig::default()
            };
            let engine = WispEngine::new(configuration.clone()).map_err(PreInitError::Trapped)?;
            let request = Initialization {
                config: req.init_blob.clone(),
                sandbox: self.pal,
                cpu_ms: DEFAULT_INITIALIZATION_CPU_MS,
                wall_ms: DEFAULT_INITIALIZATION_WALL_MS,
                max_passes: DEFAULT_INITIALIZATION_PASSES,
                log_bytes: DEFAULT_INITIALIZATION_LOG_BYTES,
            };
            let output = crate::initialize(&engine, wasm, &request).map_err(map_error)?;
            let digest = BuildRequest {
                engine: configuration,
                application_files: PlatformFiles::default(),
                runtime: RuntimeProfile::RawWasm,
                limits: Limits {
                    module_bytes_compressed: 52428800,
                    instance_memory_max: 536870912,
                    cpu_ms_per_http_dispatch: 300000,
                    subrequests_per_dispatch: 10000,
                    pending_futures_per_dispatch: 4096,
                },
                initialization: request,
                input_hash: req.module_hash.0,
            }
            .identity()
            .map_err(|error| PreInitError::Trapped(error.into()))?
            .initialization_hash;
            Ok(PreInitOutcome {
                template_bytes: output.wasm.len() as u64,
                template_wasm: output.wasm,
                deferred_units: output.deferred,
                passes: output.passes,
                init_cpu_ms: output.preparation_cpu_ns / 1_000_000,
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
                            .saturating_mul(wisp_abi::WASM_PAGE_SIZE as u64),
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

    #[test]
    fn initialization_refuses_a_request_bound_to_another_source() {
        let wasm =
            wat::parse_str("(module (func (export \"wisp_initialize\")))").expect("valid module");
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
