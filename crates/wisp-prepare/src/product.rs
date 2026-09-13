use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use wisp_engine::prepared::PreparedIdentity;
use wisp_engine::WispEngine;

use crate::{BuildRequest, BuildStage, Initialized, PrepareError, WorkerFailure};

/// Complete preparation output and the actual work recorded by its producer.
#[derive(Serialize, Deserialize)]
pub struct BuildProduct {
    /// Initialized portable state and replay observations.
    pub initialized: Initialized,
    /// Initialization, compilation and state-verification wall time.
    pub preinit_ms: f64,
    /// Final native compilation and prepared-state encoding wall time.
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
    /// Initialize exactly once per successful replay and return complete prepared output.
    async fn prepare(
        &self,
        wasm: &[u8],
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError>;
}

/// Local preparation using the shared sandbox engine and initialization implementation.
/// Guest initialization remains subject to supplied capability and resource limits.
pub struct LocalPreparation {
    pool: Arc<rayon::ThreadPool>,
    cache: Option<Arc<wisp_engine::CompilationCache>>,
}

impl LocalPreparation {
    /// Create one compilation thread so the preparation CPU report covers its work.
    pub fn new() -> Result<Self, PrepareError> {
        Self::build(None)
    }

    /// Reuse compiler-owned, uninitialized input across deployment configurations and seeds.
    pub fn with_cache(cache: Arc<wisp_engine::CompilationCache>) -> Result<Self, PrepareError> {
        Self::build(Some(cache))
    }

    fn build(cache: Option<Arc<wisp_engine::CompilationCache>>) -> Result<Self, PrepareError> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .map_err(|error| PrepareError::State(error.into()))?;
        Ok(Self {
            pool: Arc::new(pool),
            cache,
        })
    }
}

#[async_trait::async_trait]
impl Preparation for LocalPreparation {
    async fn prepare(
        &self,
        wasm: &[u8],
        request: &BuildRequest,
        cancellation: &CancellationToken,
    ) -> Result<BuildProduct, PrepareError> {
        if cancellation.is_cancelled() {
            return Err(PrepareError::Cancelled);
        }
        let wasm = wasm.to_vec();
        let request = request.clone();
        let pool = self.pool.clone();
        let cache = self.cache.clone();
        let result = tokio::task::spawn_blocking(move || {
            pool.install(|| prepare(&wasm, &request, cache.as_deref()))
        })
        .await
        .map_err(|error| PrepareError::State(error.into()))?
        .map_err(PrepareError::Rejected)?;
        if cancellation.is_cancelled() {
            return Err(PrepareError::Cancelled);
        }
        Ok(result)
    }
}

/// Run preparation in the caller's process under supplied sandbox limits.
pub fn prepare_in_current_process(
    wasm: &[u8],
    request: &BuildRequest,
) -> Result<BuildProduct, WorkerFailure> {
    prepare(wasm, request, None)
}

fn prepare(
    wasm: &[u8],
    request: &BuildRequest,
    cache: Option<&wisp_engine::CompilationCache>,
) -> Result<BuildProduct, WorkerFailure> {
    let identity =
        validate(wasm, request).map_err(|error| WorkerFailure::new(BuildStage::Input, error))?;
    let engine = WispEngine::new(request.engine.clone())
        .map_err(|error| WorkerFailure::new(BuildStage::Engine, error))?;
    let start = Instant::now();
    let initialized = crate::initialize::initialize_with_cache(
        &engine,
        wasm,
        &request.initialization,
        cache,
        &request.application_files,
        Some(request.guest_memory_bytes.map_or(
            request.limits.instance_memory_max,
            wisp_abi::config::GuestMemoryBytes::get,
        )),
    )
    .map_err(|error| WorkerFailure::new(BuildStage::Initialization, error.into()))?;
    let preinit_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let prepared = encode(
        &engine,
        &initialized.wasm,
        identity,
        request.application_files.clone(),
    )
    .map_err(|error| WorkerFailure::new(BuildStage::Artifact, error))?;
    let compile_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(BuildProduct {
        initialized,
        preinit_ms,
        compile_ms,
        prepared,
    })
}

fn validate(wasm: &[u8], request: &BuildRequest) -> anyhow::Result<PreparedIdentity> {
    anyhow::ensure!(
        *blake3::hash(wasm).as_bytes() == request.input_hash,
        "preparation input identity mismatch"
    );
    anyhow::ensure!(
        request
            .guest_memory_bytes
            .is_none_or(|limit| limit.get() <= request.engine.max_memory_bytes),
        "guest memory budget exceeds the selected worker engine capacity"
    );
    wisp_runtime::validate_with_limits(
        wasm,
        request.limits,
        request.guest_memory_bytes,
        &request.validation_limits,
    )
    .map_err(|violations| anyhow::anyhow!("portable input rejected: {violations:?}"))?;
    Ok(request.identity()?)
}

fn encode(
    engine: &WispEngine,
    wasm: &[u8],
    identity: PreparedIdentity,
    files: wisp_engine::PlatformFiles,
) -> anyhow::Result<Vec<u8>> {
    Ok(engine
        .prepare_template_with_files(wasm, identity, files)?
        .encode_builder_output()?)
}
