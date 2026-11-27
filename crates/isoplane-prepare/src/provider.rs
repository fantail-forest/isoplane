//! Preparation as a provider a node selects, run on a thread of its own.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::{BuildProduct, BuildRequest, PrepareError};

/// Produces initialized state under supplied sandbox limits.
#[async_trait::async_trait]
pub trait Preparation: Send + Sync {
    /// Initialize the module `request` carries, one that carries its whole
    /// application, and return what it leaves as one layer.
    #[cfg(feature = "rust-guests")]
    async fn prepare(
        &self,
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError>;

    /// Initialize only the runtime stage of `request`'s module and return it
    /// as the baseline `request` names. Fails with [`PrepareError::Baseline`]
    /// if that stage reads entropy or reaches a capability the sandbox refuses.
    async fn prepare_runtime_baseline(
        &self,
        request: &crate::BaselineRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError>;

    /// Initialize `request`'s application over `baseline` and return only
    /// what it changed, as a layer that loads over that baseline. Nothing is
    /// compiled. Fails with [`PrepareError::Engine`] if `request`'s engine
    /// cannot be constructed, and with [`PrepareError::Input`] unless
    /// `baseline` has its recorded digest and is the baseline `request` derives.
    async fn prepare_over(
        &self,
        baseline: &crate::PublishedBaseline,
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError>;
}

/// Local preparation using the shared sandbox engine and initialization implementation.
/// Guest initialization remains subject to supplied capability and resource limits.
pub struct LocalPreparation {
    pool: Arc<rayon::ThreadPool>,
    cache: Arc<isoplane_engine::CompilationCache>,
}

impl LocalPreparation {
    /// Create one compilation thread so the preparation CPU report covers its work,
    /// reusing compiler-owned, uninitialized input across deployment configurations.
    pub fn with_cache(cache: Arc<isoplane_engine::CompilationCache>) -> Result<Self, PrepareError> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .map_err(|error| PrepareError::State(error.into()))?;
        Ok(Self {
            pool: Arc::new(pool),
            cache,
        })
    }

    /// Run `job` on the preparation thread with this provider's cache,
    /// observing `cancellation` before and after it.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        cancellation: &CancellationToken,
        job: impl FnOnce(&isoplane_engine::CompilationCache) -> Result<T, PrepareError> + Send + 'static,
    ) -> Result<T, PrepareError> {
        if cancellation.is_cancelled() {
            return Err(PrepareError::Cancelled);
        }
        let pool = self.pool.clone();
        let cache = self.cache.clone();
        let result = tokio::task::spawn_blocking(move || pool.install(|| job(&cache)))
            .await
            .map_err(|error| PrepareError::State(error.into()))??;
        if cancellation.is_cancelled() {
            return Err(PrepareError::Cancelled);
        }
        Ok(result)
    }
}

#[async_trait::async_trait]
impl Preparation for LocalPreparation {
    #[cfg(feature = "rust-guests")]
    async fn prepare(
        &self,
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError> {
        let request = request.clone();
        self.run(cancellation, move |cache| prepare(&request, cache))
            .await
    }

    async fn prepare_runtime_baseline(
        &self,
        request: &crate::BaselineRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError> {
        let request = request.clone();
        self.run(cancellation, move |cache| {
            Ok(crate::RuntimeBaseline::prepare(&request, cache)?.product)
        })
        .await
    }

    async fn prepare_over(
        &self,
        baseline: &crate::PublishedBaseline,
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError> {
        let baseline = baseline.clone();
        let request = request.clone();
        self.run(cancellation, move |cache| {
            let loaded = baseline.load(&request)?;
            Ok(loaded
                .prepare_application_in(&request, cache.directory())?
                .product)
        })
        .await
    }
}

#[cfg(feature = "rust-guests")]
fn prepare(
    request: &BuildRequest,
    cache: &isoplane_engine::CompilationCache,
) -> Result<BuildProduct, PrepareError> {
    let identity = crate::product::validate(request)?;
    let wasm = request.module.wasm();
    let engine = isoplane_engine::IsoplaneEngine::new(request.engine.clone())
        .map_err(PrepareError::Engine)?;
    let capture = crate::initialize::CaptureMode::Layered {
        wasm,
        cache,
        identity,
    };
    Ok(crate::product::produce(&engine, request, capture)?.0)
}
