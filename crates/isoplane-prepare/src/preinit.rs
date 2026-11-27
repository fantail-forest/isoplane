//! Portable SDK initialization through the shared execution engine.

use crate::{Initialization, InitializedModule, PrepareError, RuntimeProfile};
use isoplane_abi::{
    config::{GuestMemoryBytes, InitSandbox},
    imports,
};
use isoplane_engine::{EngineConfig, IsoplaneEngine, ModuleHash, PlatformFiles};

/// Default local initialization occupancy budget across replay passes.
pub const DEFAULT_INITIALIZATION_OCCUPANCY_MS: u32 = 30_000;
/// Default local initialization wall backstop, including waits and replay passes.
pub const DEFAULT_INITIALIZATION_WALL_MS: u32 = 60_000;
/// Default local initialization replay limit.
#[cfg(feature = "rust-guests")]
pub const DEFAULT_INITIALIZATION_PASSES: u32 = 16;
/// Default retained initialization log bytes.
pub const DEFAULT_INITIALIZATION_LOG_BYTES: usize = 64 * 1024;

/// Configures portable initialization for local SDK consumers.
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
    /// Serialized configuration a Rust guest's initializer reads with `init_config_get`.
    #[cfg(feature = "rust-guests")]
    pub init_blob: Vec<u8>,
}

impl PreInitRequest {
    /// A build of the module with this digest, with no `init_config_get` configuration.
    pub fn new(module_hash: ModuleHash) -> Self {
        Self {
            module_hash,
            #[cfg(feature = "rust-guests")]
            init_blob: Vec::new(),
        }
    }
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
    /// Initialization called an import it may not, outside any unit a host
    /// built for Rust guests could defer.
    #[cfg_attr(
        feature = "rust-guests",
        error(
            "pre-init trapped outside any init unit (`{import}`); wrap initialization that may \
             touch host capabilities in an init unit: {cause}"
        )
    )]
    #[cfg_attr(
        not(feature = "rust-guests"),
        error(
            "initialization called `{import}`, which an application may not call while it \
             initializes; move that operation out of initialization: {cause}"
        )
    )]
    Unattributable {
        import: String,
        cause: anyhow::Error,
    },
    /// A deferred unit ignored the replay decision.
    #[cfg(feature = "rust-guests")]
    #[error(
        "init unit {unit} trapped on `{import}` again after being deferred; the guest is \
         ignoring `init_unit_deferred`"
    )]
    DeferralIgnored { unit: u32, import: String },
    /// Initialization exceeded its replay limit.
    #[cfg(feature = "rust-guests")]
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
    /// Select the caller's initialization capability policy, with no platform files.
    pub fn new(pal: InitSandbox) -> Self {
        Self {
            pal,
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
    pub fn run(
        &self,
        wasm: &[u8],
        req: &PreInitRequest,
    ) -> Result<InitializedModule, PreInitError> {
        if req.module_hash != ModuleHash::of(wasm) {
            return Err(PreInitError::Trapped(anyhow::anyhow!(
                "initialization source digest mismatch"
            )));
        }
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
                #[cfg(feature = "rust-guests")]
                config: req.init_blob.clone(),
                ..Initialization::new(
                    self.pal,
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
        PrepareError::Unattributable { import, detail, .. } => PreInitError::Unattributable {
            import,
            cause: anyhow::anyhow!(detail),
        },
        #[cfg(feature = "rust-guests")]
        PrepareError::DeferralIgnored { unit, import } => {
            PreInitError::DeferralIgnored { unit, import }
        }
        #[cfg(feature = "rust-guests")]
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
                PreInitializer::new(InitSandbox::Sealed).run(&wasm, &request(&wasm)),
                Err(PreInitError::MemoryLimit { .. })
            ),
            "the existing default must retain its initialization memory ceiling"
        );
        let output = PreInitializer::new(InitSandbox::Sealed)
            .with_memory_limit(GuestMemoryBytes::new(1 << 30).unwrap())
            .run(&wasm, &request(&wasm))
            .unwrap();
        assert_eq!(
            output.initialized.passes, 1,
            "a configured larger engine must initialize a bounded module above the default ceiling"
        );
    }

    #[test]
    fn local_initialization_enforces_the_consuming_engine_capacity_during_growth() {
        let wasm = memory_module(
            "1 65536",
            "(if (i32.ne (memory.grow (i32.const 1)) (i32.const -1)) (then unreachable))",
        );
        let output = PreInitializer::new(InitSandbox::Sealed)
            .with_runtime_profile(RuntimeProfile::SharedRuntime {
                sdk_version: "test".into(),
            })
            .with_memory_limit(GuestMemoryBytes::new(65536).unwrap())
            .run(&wasm, &request(&wasm))
            .unwrap();
        assert_eq!(
            output.initialized.passes, 1,
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
            PreInitializer::new(InitSandbox::Sealed)
                .run(&wasm, &request)
                .is_err(),
            "a template request must identify its exact source"
        );
    }
}
