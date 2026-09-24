//! Inputs and identity for local application preparation.

use isoplane_engine::EngineConfig;
use serde::{Deserialize, Serialize};

use crate::{Initialization, RuntimeProfile};

/// Immutable inputs to local application preparation.
#[derive(Clone, Serialize, Deserialize)]
pub struct BuildRequest {
    /// Engine settings supplied by the consuming engine.
    pub engine: EngineConfig,
    /// Immutable application files, separate from node-owned platform files.
    pub application_files: isoplane_engine::PlatformFiles,
    /// Provenance established by the trusted publisher.
    pub runtime: RuntimeProfile,
    /// Numeric application budgets enforced by portable WASM validation.
    pub limits: isoplane_abi::config::Limits,
    /// Independently authorized guest memory ceiling.
    pub guest_memory_bytes: Option<isoplane_abi::config::GuestMemoryBytes>,
    /// Structural module bounds supplied by the consuming operator.
    pub validation_limits: isoplane_abi::config::ValidationLimits,
    /// Private initialization inputs and budgets, including deployment secrets.
    pub initialization: Initialization,
    /// Digest of the submitted portable module.
    pub input_hash: [u8; 32],
    /// Whether the product also carries the initialized portable module.
    /// It does not affect the prepared identity.
    pub portable_template: PortableTemplate,
}

/// Whether a preparation also returns the initialized portable module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortableTemplate {
    /// Return it, for a consumer that registers or recompiles the portable template.
    Returned,
    /// Omit it. Layered preparation then bakes no portable module; its
    /// restored layer is still checked against the initialized instance.
    Omitted,
}

impl BuildRequest {
    /// Bind the prepared state to its input, runtime and exact initialization-visible files.
    pub fn identity(&self) -> Result<isoplane_engine::prepared::PreparedIdentity, postcard::Error> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"isoplane.prepared-initialization.v1\0");
        hash.update(&self.initialization.digest(self.input_hash)?);
        hash.update(&postcard::to_allocvec(&self.limits)?);
        hash.update(&postcard::to_allocvec(&self.guest_memory_bytes)?);
        hash.update(&postcard::to_allocvec(&self.validation_limits)?);
        hash.update(&self.engine.max_memory_bytes.to_le_bytes());
        hash.update(&postcard::to_allocvec(&self.engine.platform_files)?);
        hash.update(&postcard::to_allocvec(&self.application_files)?);
        Ok(isoplane_engine::prepared::PreparedIdentity {
            input_hash: self.input_hash,
            runtime_profile: self.runtime.digest()?,
            initialization_hash: *hash.finalize().as_bytes(),
        })
    }
}
