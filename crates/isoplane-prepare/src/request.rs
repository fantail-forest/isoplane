//! Application inputs and their preparation identity.

use std::sync::Arc;

use isoplane_engine::EngineConfig;

use crate::{Initialization, RuntimeProfile};

/// A module submitted for preparation, hashed once to identify everything
/// prepared from it. Clones share it.
#[derive(Clone)]
pub struct Module {
    wasm: Arc<[u8]>,
    digest: [u8; 32],
}

impl Module {
    /// Takes a copy of `wasm` for preparation.
    pub fn new(wasm: &[u8]) -> Self {
        Self {
            wasm: wasm.into(),
            digest: *blake3::hash(wasm).as_bytes(),
        }
    }

    pub(crate) fn wasm(&self) -> &[u8] {
        &self.wasm
    }
}

/// Immutable application inputs and execution policy supplied by the local host.
#[derive(Clone)]
pub struct BuildRequest {
    /// Engine settings supplied by the consuming host.
    pub engine: EngineConfig,
    /// Immutable application files, separate from node-owned platform files.
    pub application_files: isoplane_engine::PlatformFiles,
    /// Provenance established by the trusted publisher.
    pub runtime: RuntimeProfile,
    /// Soft memory allowance when no explicit guest-memory override is supplied.
    pub memory_limit: isoplane_abi::config::GuestMemoryBytes,
    /// Independently authorized guest memory ceiling.
    pub guest_memory_bytes: Option<isoplane_abi::config::GuestMemoryBytes>,
    /// Structural module bounds supplied by the consuming operator.
    pub validation_limits: isoplane_abi::config::ValidationLimits,
    /// Private initialization inputs and budgets, including deployment secrets.
    pub initialization: Initialization,
    /// The module preparation initializes.
    pub module: Module,
}

impl BuildRequest {
    /// Bind the prepared state to its input, runtime and the content of the
    /// files initialization can read. This input identity permits reuse of captured
    /// state; a fresh execution may draw different entropy. File times are not bound:
    /// a file set is identified by its paths and bytes.
    pub fn identity(&self) -> Result<isoplane_engine::prepared::PreparedIdentity, postcard::Error> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"isoplane.prepared-initialization.v4\0");
        hash.update(&self.initialization.digest(self.module.digest)?);
        hash.update(&postcard::to_allocvec(&self.memory_limit)?);
        hash.update(&postcard::to_allocvec(&self.guest_memory_bytes)?);
        hash.update(&postcard::to_allocvec(&self.validation_limits)?);
        hash.update(&self.engine.max_memory_bytes.to_le_bytes());
        hash.update(&self.engine.platform_files.content_digest());
        hash.update(&self.application_files.content_digest());
        Ok(isoplane_engine::prepared::PreparedIdentity {
            input_hash: self.module.digest,
            runtime_profile: self.runtime.digest()?,
            initialization_hash: *hash.finalize().as_bytes(),
        })
    }
}
