//! A runtime prepared once to the end of its own initializer, and
//! applications prepared as layers over it.

use std::sync::Arc;

use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::{IsoplaneEngine, Template};

use crate::product::{produce, validate};
use crate::{BuildProduct, BuildRequest, Initialization, PrepareError};

/// What a runtime's own initialization sees: an application's request
/// without its files, deployment configuration, network or entropy, so every
/// application prepared with the same runtime, engine and limits shares it.
#[derive(Clone)]
pub struct BaselineRequest(BuildRequest);

impl BuildRequest {
    /// The runtime baseline this request's application is prepared over.
    pub fn baseline(&self) -> BaselineRequest {
        let initialization = &self.initialization;
        BaselineRequest(BuildRequest {
            engine: self.engine.clone(),
            application_files: isoplane_engine::PlatformFiles::default(),
            runtime: self.runtime.clone(),
            memory_limit: self.memory_limit,
            guest_memory_bytes: self.guest_memory_bytes,
            validation_limits: self.validation_limits,
            initialization: Initialization {
                network: None,
                runtime_config: b"{}".to_vec(),
                occupancy_ms: initialization.occupancy_ms,
                wall_ms: initialization.wall_ms,
                log_bytes: initialization.log_bytes,
            },
            module: self.module.clone(),
        })
    }
}

impl BaselineRequest {
    /// The identity the baseline is prepared, published and loaded under.
    pub fn identity(&self) -> Result<PreparedIdentity, postcard::Error> {
        self.0.identity()
    }
}

/// A runtime baseline published in a directory, identified by the manifest
/// digest recorded by local selection.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct PublishedBaseline {
    directory: std::path::PathBuf,
    manifest_sha256: [u8; 32],
}

#[cfg(target_os = "linux")]
impl PublishedBaseline {
    /// # Safety
    /// The directory must contain output from the controlled local compiler.
    /// Nothing may change or remove the files in `directory` while any
    /// preparation or template uses the baseline: preparation maps them and
    /// runs the native code they hold.
    pub unsafe fn new(directory: std::path::PathBuf, manifest_sha256: [u8; 32]) -> Self {
        Self {
            directory,
            manifest_sha256,
        }
    }

    /// Loads the baseline for preparing `request`'s application over it.
    /// Fails with [`PrepareError::Engine`] if `request`'s engine cannot be
    /// constructed, and with [`PrepareError::Input`] unless the manifest has
    /// the recorded digest and names the baseline `request` derives, for
    /// that engine.
    pub fn load(&self, request: &BuildRequest) -> Result<RuntimeBaseline, PrepareError> {
        let baseline = request.baseline();
        let engine = IsoplaneEngine::new(request.engine.clone()).map_err(PrepareError::Engine)?;
        let identity = baseline.identity().map_err(PrepareError::Encoding)?;
        // SAFETY: `PublishedBaseline::new`'s caller keeps the directory's
        // files unchanged while any preparation or template uses them.
        let template = unsafe {
            engine.restore_prepared_by_digest(&self.directory, self.manifest_sha256, identity)
        }
        .map_err(|error| PrepareError::Input(error.into()))?;
        RuntimeBaseline::loaded(Arc::new(engine), template, &baseline)
    }
}

/// A runtime prepared to the end of its runtime-only initializer and loaded
/// for preparing applications over it. Clones share it.
#[derive(Clone)]
pub struct RuntimeBaseline(Arc<Loaded>);

struct Loaded {
    engine: Arc<IsoplaneEngine>,
    template: Template,
    identity: PreparedIdentity,
}

/// A runtime baseline as preparation produced it.
pub struct PreparedBaseline {
    /// The baseline, loaded for preparing applications over it.
    pub baseline: RuntimeBaseline,
    /// The baseline as preparation output: what a consumer publishes and
    /// loads before loading any application layer over it.
    pub product: BuildProduct,
}

/// An application prepared over a runtime baseline.
pub struct PreparedApplication {
    /// The layer as preparation output, which loads over the published baseline.
    pub product: BuildProduct,
    /// The layer already loaded over the baseline in the baseline's engine,
    /// as preparation verified it.
    pub template: Template,
}

impl RuntimeBaseline {
    /// Initialize only `wasm`'s runtime, through its `isoplane_initialize_runtime`
    /// export, in the caller's thread, compiling through `cache`.
    /// Fails with [`PrepareError::Baseline`] if that initialization reads entropy
    /// or reaches a capability the sandbox refuses, since every application
    /// over the baseline would then share its result.
    pub fn prepare(
        request: &BaselineRequest,
        cache: &isoplane_engine::CompilationCache,
    ) -> Result<PreparedBaseline, PrepareError> {
        let request = &request.0;
        let identity = validate(request)?;
        let wasm = request.module.wasm();
        let engine = IsoplaneEngine::new(request.engine.clone()).map_err(PrepareError::Engine)?;
        let capture = crate::initialize::CaptureMode::RuntimeBaseline {
            wasm,
            cache,
            identity,
        };
        let (product, template) = produce(&engine, request, capture)?;
        let template = template.ok_or_else(|| {
            PrepareError::InvalidRequest("a runtime baseline needs a module with an initializer")
        })?;
        Ok(PreparedBaseline {
            baseline: Self(Arc::new(Loaded {
                engine: Arc::new(engine),
                template,
                identity,
            })),
            product,
        })
    }

    /// A baseline published from [`RuntimeBaseline::prepare`]'s output and
    /// loaded by the caller into `engine` under `request`'s identity. Fails
    /// if `template` is not a runtime baseline, which still runs the
    /// application's initializer.
    pub fn loaded(
        engine: Arc<IsoplaneEngine>,
        template: Template,
        request: &BaselineRequest,
    ) -> Result<Self, PrepareError> {
        if !template.has_initialize() {
            return Err(PrepareError::InvalidRequest(
                "a runtime baseline must still run the application's initializer",
            ));
        }
        let identity = request.identity().map_err(PrepareError::Encoding)?;
        Ok(Self(Arc::new(Loaded {
            engine,
            template,
            identity,
        })))
    }

    /// Initialize `request`'s application on this baseline in the caller's
    /// thread and capture only what it changed, as a layer over it, keeping
    /// temporary captured state in `storage`, an existing directory the
    /// caller owns. Runs no compilation. Fails with [`PrepareError::Input`]
    /// unless this baseline was prepared from `request.baseline()`.
    pub fn prepare_application_in(
        &self,
        request: &BuildRequest,
        storage: &std::path::Path,
    ) -> Result<PreparedApplication, PrepareError> {
        let identity = layer_identity(self, request)?;
        let capture = crate::initialize::CaptureMode::Over {
            base: self.0.template.clone(),
            storage,
            identity,
        };
        let (product, template) = produce(&self.0.engine, request, capture)?;
        let template = template.ok_or_else(|| {
            PrepareError::InvalidRequest(
                "an application over a runtime baseline needs its initializer",
            )
        })?;
        Ok(PreparedApplication { product, template })
    }

    /// The identity the baseline was prepared under.
    pub fn identity(&self) -> PreparedIdentity {
        self.0.identity
    }

    /// The engine the baseline and every [`PreparedApplication::template`]
    /// over it are loaded in.
    pub fn engine(&self) -> &Arc<IsoplaneEngine> {
        &self.0.engine
    }

    /// The baseline as a template, which an application layer prepared over
    /// it is loaded over.
    pub fn template(&self) -> &Template {
        &self.0.template
    }
}

fn layer_identity(
    baseline: &RuntimeBaseline,
    request: &BuildRequest,
) -> Result<PreparedIdentity, PrepareError> {
    let derived = request
        .baseline()
        .identity()
        .map_err(PrepareError::Encoding)?;
    if derived != baseline.0.identity {
        return Err(PrepareError::Input(anyhow::anyhow!(
            "the runtime baseline was prepared for a different runtime, engine or limits"
        )));
    }
    request.identity().map_err(PrepareError::Encoding)
}
