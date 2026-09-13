//! Inputs and identity for local application preparation.

use serde::{Deserialize, Serialize};
use wisp_engine::EngineConfig;

use crate::{Initialization, RuntimeProfile};

/// Immutable inputs to local application preparation.
#[derive(Clone, Serialize, Deserialize)]
pub struct BuildRequest {
    /// Engine settings supplied by the consuming engine.
    pub engine: EngineConfig,
    /// Immutable application files, separate from node-owned platform files.
    pub application_files: wisp_engine::PlatformFiles,
    /// Provenance established by the trusted publisher.
    pub runtime: RuntimeProfile,
    /// Numeric application budgets enforced by portable WASM validation.
    pub limits: wisp_abi::config::Limits,
    /// Independently authorized guest memory ceiling.
    pub guest_memory_bytes: Option<wisp_abi::config::GuestMemoryBytes>,
    /// Non-secret initialization inputs and budgets.
    pub initialization: Initialization,
    /// Digest of the submitted portable module.
    pub input_hash: [u8; 32],
}

impl BuildRequest {
    /// Bind the prepared state to its input, runtime and exact initialization-visible files.
    pub fn identity(&self) -> Result<wisp_engine::prepared::PreparedIdentity, postcard::Error> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"wisp.prepared-initialization.v1\0");
        hash.update(&self.initialization.digest(self.input_hash)?);
        hash.update(&postcard::to_allocvec(&self.guest_memory_bytes)?);
        hash.update(&postcard::to_allocvec(&self.engine.platform_files)?);
        hash.update(&postcard::to_allocvec(&self.application_files)?);
        Ok(wisp_engine::prepared::PreparedIdentity {
            input_hash: self.input_hash,
            runtime_profile: self.runtime.digest()?,
            initialization_hash: *hash.finalize().as_bytes(),
        })
    }
}
