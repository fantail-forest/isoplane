//! Preparation output, and the admission and capture every way of
//! preparing shares.

use std::time::Instant;

use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::IsoplaneEngine;

use crate::{BuildRequest, Initialized, PrepareError};

/// Complete preparation output and the actual work recorded by its producer.
pub struct BuildProduct {
    /// What initialization observed and the replay state it leaves.
    pub initialized: Initialized,
    /// Initialization, compilation and state-verification wall time.
    pub preinit_ms: f64,
    /// Native output encoding.
    pub compile_ms: f64,
    prepared: Vec<u8>,
}

impl BuildProduct {
    /// Native and explicit-state transport for the matching engine's trusted decoder.
    pub fn prepared_bytes(&self) -> &[u8] {
        &self.prepared
    }
}

/// Initialize `request` under `engine` and encode the captured state,
/// with the verified template it loads back to.
pub(crate) fn produce(
    engine: &IsoplaneEngine,
    request: &BuildRequest,
    capture: crate::initialize::CaptureMode<'_>,
) -> Result<(BuildProduct, Option<isoplane_engine::Template>), PrepareError> {
    let start = Instant::now();
    let output = crate::initialize::initialize(
        engine,
        &request.initialization,
        &request.application_files,
        Some(request.guest_memory_bytes.map_or(
            request.memory_limit.get(),
            isoplane_abi::config::GuestMemoryBytes::get,
        )),
        capture,
    )?;
    let preinit_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let prepared = output
        .prepared
        .encode_builder_output()
        .map_err(|error| PrepareError::Artifact(error.into()))?;
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
