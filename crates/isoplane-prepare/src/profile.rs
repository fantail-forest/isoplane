//! Trusted producer provenance, independent of adjustable tenant quotas.

use serde::{Deserialize, Serialize};

/// Provenance supplied by the trusted build pipeline, never inferred from client WASM.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeProfile {
    /// Raw WASM admission makes no managed-runtime or hosted IL claim.
    #[default]
    RawWasm,
    /// The shared runtime a node installs for one SDK release, which
    /// application packages built with that release run on.
    SharedRuntime {
        /// The SDK release that carries the runtime.
        sdk_version: String,
    },
}

impl RuntimeProfile {
    /// Profile for explicitly raw WASM, without invented managed provenance.
    pub fn raw_wasm() -> Self {
        Self::RawWasm
    }

    /// The memory maximum a module of this profile may declare, given its
    /// application's limit: that limit, except for a shared runtime, which
    /// can declare the complete wasm32 address range to serve applications
    /// with different limits. Initialization and execution hold every instance to its
    /// application's limit either way.
    pub fn declared_memory_ceiling(
        &self,
        application: Option<isoplane_abi::config::GuestMemoryBytes>,
    ) -> Option<isoplane_abi::config::GuestMemoryBytes> {
        match self {
            Self::SharedRuntime { .. } => Some(isoplane_abi::config::GuestMemoryBytes::WASM32_MAX),
            Self::RawWasm => application,
        }
    }

    /// Digest of the runtime profile recorded in prepared metadata.
    pub fn digest(&self) -> Result<[u8; 32], postcard::Error> {
        let mut hash = blake3::Hasher::new();
        hash.update(b"isoplane.runtime-profile.v1\0");
        hash.update(&postcard::to_allocvec(self)?);
        Ok(*hash.finalize().as_bytes())
    }
}
