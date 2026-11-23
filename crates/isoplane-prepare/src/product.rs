use std::sync::Arc;
use std::time::Instant;

use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::IsoplaneEngine;
use tokio_util::sync::CancellationToken;

use crate::{BuildRequest, Initialized, PrepareError};

/// Complete preparation output and the actual work recorded by its producer.
pub struct BuildProduct {
    /// What initialization observed and the replay state it leaves.
    pub initialized: Initialized,
    /// Initialization, compilation and state-verification wall time.
    pub preinit_ms: f64,
    /// Native output encoding and any final compilation of portable captured state.
    pub compile_ms: f64,
    prepared: Vec<u8>,
}

impl BuildProduct {
    /// Native and explicit-state transport for the matching engine's trusted decoder.
    pub fn prepared_bytes(&self) -> &[u8] {
        &self.prepared
    }
}

/// Produces initialized state under supplied sandbox limits.
#[async_trait::async_trait]
pub trait Preparation: Send + Sync {
    /// Run bounded initialization attempts and return the successful captured output.
    #[cfg(any(not(target_os = "linux"), feature = "rust-guests"))]
    async fn prepare(
        &self,
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError>;

    /// Initialize only the runtime stage of `request`'s module and return it
    /// as the baseline `request` names. Fails with [`PrepareError::Baseline`]
    /// if that stage reads entropy or reaches a capability the sandbox refuses.
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
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
    #[cfg(any(not(target_os = "linux"), feature = "rust-guests"))]
    async fn prepare(
        &self,
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError> {
        let request = request.clone();
        self.run(cancellation, move |cache| prepare(&request, Some(cache)))
            .await
    }

    #[cfg(target_os = "linux")]
    async fn prepare_runtime_baseline(
        &self,
        request: &crate::BaselineRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError> {
        let request = request.clone();
        self.run(cancellation, move |cache| {
            Ok(crate::RuntimeBaseline::prepare(&request, Some(cache))?.product)
        })
        .await
    }

    #[cfg(target_os = "linux")]
    async fn prepare_over(
        &self,
        baseline: &crate::PublishedBaseline,
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError> {
        let baseline = baseline.clone();
        let request = request.clone();
        self.run(cancellation, move |_| {
            let loaded = baseline.load(&request)?;
            Ok(loaded.prepare_application(&request)?.product)
        })
        .await
    }
}

#[cfg(any(not(target_os = "linux"), feature = "rust-guests"))]
fn prepare(
    request: &BuildRequest,
    cache: Option<&isoplane_engine::CompilationCache>,
) -> Result<BuildProduct, PrepareError> {
    let identity = validate(request)?;
    let wasm = request.module.wasm();
    let engine = IsoplaneEngine::new(request.engine.clone()).map_err(PrepareError::Engine)?;
    #[cfg(all(target_os = "linux", feature = "rust-guests"))]
    let capture = crate::initialize::CaptureMode::Layered { wasm, identity };
    #[cfg(not(all(target_os = "linux", feature = "rust-guests")))]
    let capture = crate::initialize::CaptureMode::Portable { wasm };
    Ok(produce(&engine, request, cache, identity, capture)?.0)
}

/// Initialize `request` under `engine` and encode the captured state,
/// with the verified template it loads back to.
pub(crate) fn produce(
    engine: &IsoplaneEngine,
    request: &BuildRequest,
    cache: Option<&isoplane_engine::CompilationCache>,
    #[cfg_attr(
        not(any(not(target_os = "linux"), feature = "rust-guests")),
        expect(
            unused_variables,
            reason = "only a portable capture is compiled after it is captured"
        )
    )]
    identity: PreparedIdentity,
    capture: crate::initialize::CaptureMode<'_>,
) -> Result<(BuildProduct, Option<isoplane_engine::Template>), PrepareError> {
    let start = Instant::now();
    let output = crate::initialize::initialize_with_cache(
        engine,
        &request.initialization,
        cache,
        &request.application_files,
        Some(request.guest_memory_bytes.map_or(
            request.memory_limit.get(),
            isoplane_abi::config::GuestMemoryBytes::get,
        )),
        capture,
    )?;
    let preinit_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let prepared = match output.captured {
        crate::initialize::Captured::Layered { prepared } => prepared
            .encode_builder_output()
            .map_err(|error| PrepareError::Artifact(error.into()))?,
        #[cfg(any(not(target_os = "linux"), feature = "rust-guests"))]
        crate::initialize::Captured::Portable(module) => {
            encode(engine, &module, identity, request.application_files.clone())
                .map_err(PrepareError::Artifact)?
        }
    };
    let compile_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok((
        BuildProduct {
            initialized: output.initialized,
            preinit_ms,
            compile_ms,
            prepared,
        },
        output.restored,
    ))
}

pub(crate) fn validate(request: &BuildRequest) -> Result<PreparedIdentity, PrepareError> {
    let ceiling_fits = request.guest_memory_bytes.is_none_or(|limit| {
        isoplane_abi::config::memory_ceiling_bytes(limit.get()) <= request.engine.max_memory_bytes
    });
    if !ceiling_fits {
        return Err(PrepareError::Input(anyhow::anyhow!(
            "the guest memory limit's ceiling exceeds the selected worker engine capacity"
        )));
    }
    isoplane_runtime::validate_with_limits(
        request.module.wasm(),
        request.memory_limit,
        request
            .runtime
            .declared_memory_ceiling(request.guest_memory_bytes),
        &request.validation_limits,
    )
    .map_err(|violations| {
        PrepareError::Input(anyhow::anyhow!("portable input rejected: {violations:?}"))
    })?;
    request.identity().map_err(PrepareError::Encoding)
}

#[cfg(any(not(target_os = "linux"), feature = "rust-guests"))]
fn encode(
    engine: &IsoplaneEngine,
    wasm: &[u8],
    identity: PreparedIdentity,
    files: isoplane_engine::PlatformFiles,
) -> anyhow::Result<Vec<u8>> {
    Ok(engine
        .prepare_template_with_files(wasm, identity, files)?
        .encode_builder_output()?)
}
