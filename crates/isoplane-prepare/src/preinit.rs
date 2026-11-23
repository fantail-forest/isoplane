//! Portable SDK initialization through the shared execution engine.

use crate::{Initialization, InitializedModule, PrepareError, RuntimeProfile};
use isoplane_abi::{config::GuestMemoryBytes, imports};
use isoplane_engine::{EngineConfig, IsoplaneEngine, PlatformFiles};

/// Default local initialization occupancy budget.
pub const DEFAULT_INITIALIZATION_OCCUPANCY_MS: u32 = 30_000;
/// Default local initialization wall backstop, including waits.
pub const DEFAULT_INITIALIZATION_WALL_MS: u32 = 60_000;
/// Default retained initialization log bytes.
pub const DEFAULT_INITIALIZATION_LOG_BYTES: usize = 64 * 1024;

/// Configures portable initialization for local SDK consumers.
pub struct PreInitializer {
    runtime: RuntimeProfile,
    platform_files: PlatformFiles,
    memory_limit: Option<GuestMemoryBytes>,
    engine: EngineConfig,
    runtime_config: Option<Vec<u8>>,
    network: Option<crate::InitializationNetwork>,
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
    /// Initialization called an import it is not permitted.
    #[error(
        "initialization called `{import}`, which an application may not call while it \
         initializes; move that operation out of initialization: {cause}"
    )]
    ForbiddenCapability {
        import: String,
        cause: anyhow::Error,
    },
    /// The declared memory exceeds the selected initialization capacity.
    #[error("declared init memory of {bytes} bytes exceeds the init limit of {limit} bytes")]
    MemoryLimit { bytes: u64, limit: u64 },
    /// A guest `abort`, an `unreachable`, an out-of-memory.
    #[error("pre-init trapped: {0}")]
    Trapped(anyhow::Error),
}

impl Default for PreInitializer {
    fn default() -> Self {
        Self::new()
    }
}

impl PreInitializer {
    /// Local initialization with no platform files.
    pub fn new() -> Self {
        Self {
            runtime: RuntimeProfile::default(),
            platform_files: PlatformFiles::default(),
            memory_limit: None,
            engine: EngineConfig::default(),
            runtime_config: None,
            network: None,
        }
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
    pub fn run(&self, wasm: &[u8]) -> Result<InitializedModule, PreInitError> {
        let (has_initializer, bytes) = module_shape(wasm).map_err(PreInitError::Trapped)?;
        if !has_initializer {
            return Err(PreInitError::NoInitFunc(imports::exports::INITIALIZE));
        }
        let limit = self.memory_limit.map_or(
            isoplane_abi::config::Limits::default().instance_memory_max,
            GuestMemoryBytes::get,
        );
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
                ..Initialization::new(
                    self.runtime_config
                        .clone()
                        .unwrap_or_else(|| b"{}".to_vec()),
                )
            };
            crate::initialize(&engine, wasm, &request).map_err(map_error)
        })
    }
}

fn map_error(error: PrepareError) -> PreInitError {
    match error {
        PrepareError::ForbiddenCapability { import, detail, .. } => {
            PreInitError::ForbiddenCapability {
                import,
                cause: anyhow::anyhow!(detail),
            }
        }
        // Other preparation failures have no more specific local classification.
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

    #[test]
    fn local_initialization_accepts_the_explicit_engine_capacity_above_its_default() {
        let wasm = memory_module("1 16384", "");
        assert!(
            matches!(
                PreInitializer::new().run(&wasm),
                Err(PreInitError::MemoryLimit { .. })
            ),
            "the existing default must retain its initialization memory ceiling"
        );
        PreInitializer::new()
            .with_memory_limit(GuestMemoryBytes::new(1 << 30).unwrap())
            .run(&wasm)
            .expect(
                "a configured larger engine must initialize a bounded module above the default ceiling",
            );
    }

    #[test]
    fn local_initialization_enforces_the_consuming_engine_capacity_during_growth() {
        let wasm = memory_module(
            "1 65536",
            "(if (i32.ne (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable))",
        );
        PreInitializer::new()
            .with_runtime_profile(RuntimeProfile::SharedRuntime {
                sdk_version: "test".into(),
            })
            .with_memory_limit(GuestMemoryBytes::new(65536).unwrap())
            .run(&wasm)
            .expect("initialization must deny growth beyond its configured engine capacity");
    }
}
