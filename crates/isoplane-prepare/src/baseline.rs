//! A runtime prepared once to the end of its own initializer, and
//! applications prepared as layers over it.

use std::sync::Arc;

use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::{IsoplaneEngine, Template};
use tokio_util::sync::CancellationToken;

use crate::product::{produce, validate};
use crate::{
    BuildProduct, BuildRequest, BuildStage, Initialization, LocalPreparation, PortableTemplate,
    PrepareError, WorkerFailure,
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
                cpu_ms: initialization.cpu_ms,
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
}

/// A runtime prepared to the end of its runtime-only initializer and loaded
/// for preparing applications over it. Clones share it.
#[derive(Clone)]
pub struct RuntimeBaseline(Arc<Loaded>);

struct Loaded {
    engine: IsoplaneEngine,
    template: Template,
    identity: PreparedIdentity,
    product: BuildProduct,
}

impl RuntimeBaseline {
    /// The baseline as preparation output: what a consumer publishes and
    /// loads before loading any application layer over it.
    pub fn product(&self) -> &BuildProduct {
        &self.0.product
    }
}

impl LocalPreparation {
    /// Initialize only `wasm`'s runtime, through its `isoplane_initialize_runtime`
    /// export. Fails at [`BuildStage::Baseline`] if that initialization reads
    /// entropy or reaches a capability the sandbox refuses, since every
    /// application over the baseline would then share its result.
    pub async fn prepare_runtime_baseline(
        &self,
        wasm: &[u8],
        request: &BaselineRequest,
        cancellation: &CancellationToken,
    ) -> Result<RuntimeBaseline, PrepareError> {
        let wasm = wasm.to_vec();
        let request = request.0.clone();
        self.run(cancellation, move |cache| {
            let identity = validate(&wasm, &request)
                .map_err(|error| WorkerFailure::new(BuildStage::Input, error))?;
            let engine = IsoplaneEngine::new(request.engine.clone())
                .map_err(|error| WorkerFailure::new(BuildStage::Engine, error))?;
            let capture = crate::initialize::CaptureMode::RuntimeBaseline {
                wasm: &wasm,
                identity,
            };
            let (product, template) = produce(&engine, &request, cache, identity, capture)?;
            let template = template.ok_or_else(|| {
                WorkerFailure::new(
                    BuildStage::Initialization,
                    anyhow::anyhow!("a runtime baseline needs a module with an initializer"),
                )
            })?;
            Ok(RuntimeBaseline(Arc::new(Loaded {
                engine,
                template,
                identity,
                product,
            })))
        })
        .await
    }

    /// Initialize `request`'s application on `baseline` and capture only what
    /// it changed, as a layer that loads over the published baseline. Runs no
    /// compilation. Fails at [`BuildStage::Input`] unless `baseline` was
    /// prepared from `request.baseline()`, or if the request asks for a
    /// portable module, which a layer does not have.
    pub async fn prepare_over(
        &self,
        baseline: &RuntimeBaseline,
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError> {
        let baseline = baseline.clone();
        let request = request.clone();
        self.run(cancellation, move |_| {
            let identity = layer_identity(&baseline, &request)
                .map_err(|error| WorkerFailure::new(BuildStage::Input, error))?;
            let capture = crate::initialize::CaptureMode::Over {
                base: baseline.0.template.clone(),
                identity,
            };
            Ok(produce(&baseline.0.engine, &request, None, identity, capture)?.0)
        })
        .await
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
