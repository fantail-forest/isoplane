//! A runtime prepared once to the end of its own initializer, and
//! applications prepared as layers over it.

use std::sync::Arc;

use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::{IsoplaneEngine, Template};

use crate::product::{produce, validate};
use crate::{
    BuildProduct, BuildRequest, BuildStage, Initialization, PortableTemplate, PrepareError,
    WorkerFailure,
};

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
            limits: self.limits,
            guest_memory_bytes: self.guest_memory_bytes,
            validation_limits: self.validation_limits,
            initialization: Initialization {
                network: None,
                config: Vec::new(),
                runtime_config: b"{}".to_vec(),
                sandbox: initialization.sandbox,
                occupancy_ms: initialization.occupancy_ms,
                wall_ms: initialization.wall_ms,
                max_passes: 1,
                log_bytes: initialization.log_bytes,
            },
            input_hash: self.input_hash,
            portable_template: PortableTemplate::Omitted,
        })
    }
}

impl BaselineRequest {
    /// The identity the baseline is prepared, published and loaded under.
    pub fn identity(&self) -> Result<PreparedIdentity, postcard::Error> {
        self.0.identity()
    }

    /// The request as a worker receives it; its own `baseline()` is itself.
    #[cfg(target_os = "linux")]
    pub(crate) fn build_request(&self) -> &BuildRequest {
        &self.0
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
    /// Nothing may change or remove the files in `directory` while any
    /// preparation or template uses the baseline: preparation maps them and
    /// runs the native code they hold.
    pub unsafe fn new(directory: std::path::PathBuf, manifest_sha256: [u8; 32]) -> Self {
        Self {
            directory,
            manifest_sha256,
        }
    }

    /// Where the baseline is published.
    pub fn directory(&self) -> &std::path::Path {
        &self.directory
    }

    /// The digest the baseline's manifest must have.
    pub fn manifest_sha256(&self) -> [u8; 32] {
        self.manifest_sha256
    }

    /// Loads the baseline for preparing `request`'s application over it.
    /// Fails unless its manifest has the recorded digest and names the
    /// baseline `request` derives, for this engine.
    pub fn load(&self, request: &BuildRequest) -> anyhow::Result<RuntimeBaseline> {
        let baseline = request.baseline();
        let engine = IsoplaneEngine::new(request.engine.clone())?;
        // SAFETY: `PublishedBaseline::new`'s caller keeps the directory's
        // files unchanged while any preparation or template uses them.
        let template = unsafe {
            engine.restore_prepared_by_digest(
                &self.directory,
                self.manifest_sha256,
                baseline.identity()?,
            )
        }?;
        Ok(RuntimeBaseline::loaded(
            Arc::new(engine),
            template,
            &baseline,
        )?)
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
    /// export, in the caller's thread, compiling through `cache` when given.
    /// Fails at [`BuildStage::Baseline`] if that initialization reads entropy
    /// or reaches a capability the sandbox refuses, since every application
    /// over the baseline would then share its result.
    pub fn prepare(
        wasm: &[u8],
        request: &BaselineRequest,
        cache: Option<&isoplane_engine::CompilationCache>,
    ) -> Result<PreparedBaseline, WorkerFailure> {
        let request = &request.0;
        let identity = validate(wasm, request)
            .map_err(|error| WorkerFailure::new(BuildStage::Input, error))?;
        let engine = IsoplaneEngine::new(request.engine.clone())
            .map_err(|error| WorkerFailure::new(BuildStage::Engine, error))?;
        let capture = crate::initialize::CaptureMode::RuntimeBaseline { wasm, identity };
        let (product, template) = produce(&engine, request, cache, identity, capture)?;
        let template = template.ok_or_else(|| {
            WorkerFailure::new(
                BuildStage::Initialization,
                anyhow::anyhow!("a runtime baseline needs a module with an initializer"),
            )
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
    /// thread and capture only what it changed, as a layer over it. Runs no
    /// compilation. Fails at [`BuildStage::Input`] unless this baseline was
    /// prepared from `request.baseline()`, or if the request asks for a
    /// portable module, which a layer does not have.
    pub fn prepare_application(
        &self,
        request: &BuildRequest,
    ) -> Result<PreparedApplication, WorkerFailure> {
        self.prepare_application_in(request, &std::env::temp_dir())
    }

    /// Prepare an application with temporary captured state confined to `storage`.
    /// The directory must already exist and be owned by the caller.
    pub fn prepare_application_in(
        &self,
        request: &BuildRequest,
        storage: &std::path::Path,
    ) -> Result<PreparedApplication, WorkerFailure> {
        let identity = layer_identity(self, request)
            .map_err(|error| WorkerFailure::new(BuildStage::Input, error))?;
        let capture = crate::initialize::CaptureMode::Over {
            base: self.0.template.clone(),
            storage,
            identity,
        };
        let (product, template) = produce(&self.0.engine, request, None, identity, capture)?;
        let template = template.ok_or_else(|| {
            WorkerFailure::new(
                BuildStage::Initialization,
                anyhow::anyhow!("an application over a runtime baseline needs its initializer"),
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
) -> anyhow::Result<PreparedIdentity> {
    anyhow::ensure!(
        request.baseline().identity()? == baseline.0.identity,
        "the runtime baseline was prepared for a different runtime, engine or limits"
    );
    anyhow::ensure!(
        request.portable_template == PortableTemplate::Omitted,
        "a layer over a runtime baseline has no portable module"
    );
    Ok(request.identity()?)
}
