//! Validate application modules and the trusted shared runtimes they use.

use isoplane_abi::config::{AppManifest, GuestMemoryBytes, InitSandbox};
use serde::{Deserialize, Serialize};

use crate::validator::{self, ValidationReport, Violation};

/// An immutable identity recorded in a prepared application's receipt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VersionId(pub String);

impl std::fmt::Display for VersionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Owner identity and initialization policy supplied by the local host.
#[derive(Debug, Clone)]
pub struct DeployOptions {
    /// Opaque identity of this preparation, independent of application routing.
    pub version: VersionId,
    /// Established owner associated with application settings and network policy.
    pub tenant: String,
    /// Capabilities available during initialization.
    pub init_sandbox: InitSandbox,
}

/// Admission failed before initialization or native compilation.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    /// The module violates the sandbox contract or its configured limits.
    #[error("validation rejected the module ({} violation(s)):\n{}", .0.len(),
            .0.iter().map(|v| format!("  - {v}")).collect::<Vec<_>>().join("\n"))]
    Rejected(Vec<Violation>),
    /// The compressed application exceeds its operator-supplied budget.
    #[error("module is {size} bytes compressed, over the configured limit of {limit}")]
    TooLarge {
        /// Compressed module size in bytes.
        size: u64,
        /// Configured maximum compressed size in bytes.
        limit: u64,
    },
    /// Compression failed while measuring the module.
    #[error("compressing the module: {0}")]
    Compress(#[source] std::io::Error),
}

/// Validate a module and enforce its compressed-size budget before preparation.
pub fn admit(manifest: &AppManifest, wasm: &[u8]) -> Result<ValidationReport, AdmissionError> {
    let report = validator::validate_with_limits(
        wasm,
        manifest.memory_limit(),
        manifest.guest_memory_bytes,
        &manifest.validation_limits,
    )
    .map_err(AdmissionError::Rejected)?;
    let compressed = zstd::encode_all(wasm, 19).map_err(AdmissionError::Compress)?;
    let size = compressed.len() as u64;
    let limit = manifest.effective_limits().module_bytes_compressed;
    if size > limit {
        return Err(AdmissionError::TooLarge { size, limit });
    }
    Ok(report)
}

/// Validate the host-selected shared runtime with the application's structural limits.
/// Its declared memory may span wasm32; execution enforces each application's memory
/// budget. The application package has its own size check, excluding this shared code.
pub fn admit_shared_runtime(
    manifest: &AppManifest,
    wasm: &[u8],
) -> Result<ValidationReport, AdmissionError> {
    validator::validate_with_limits(
        wasm,
        manifest.memory_limit(),
        Some(GuestMemoryBytes::WASM32_MAX),
        &manifest.validation_limits,
    )
    .map_err(AdmissionError::Rejected)
}
