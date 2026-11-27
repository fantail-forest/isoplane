use serde::{Deserialize, Serialize};

/// Provenance supplied by the trusted build pipeline, never inferred from client WASM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeProfile {
    /// The shared runtime a node installs for one SDK release, which
    /// application packages built with that release run on.
    SharedRuntime {
        /// The SDK release that carries the runtime.
        sdk_version: String,
    },
    /// Raw WASM admission makes no managed-runtime or hosted IL claim. Last,
    /// so a shared runtime's digest is the same with and without the feature.
    #[cfg(feature = "rust-guests")]
    RawWasm,
}

impl RuntimeProfile {
    /// The SDK release whose shared runtime the module is, or `None` for a
    /// module that carries its whole application.
    pub fn shared_runtime_sdk_version(&self) -> Option<&str> {
        match self {
            Self::SharedRuntime { sdk_version } => Some(sdk_version),
            #[cfg(feature = "rust-guests")]
            Self::RawWasm => None,
        }
    }

    /// The memory maximum a module of this profile may declare, given its
    /// application's limit: that limit, except for a shared runtime, which
    /// can declare the complete wasm32 address range to serve applications
    /// with different limits. Initialization and execution hold every instance to its
    /// application's limit either way.
    pub fn declared_memory_ceiling(
        &self,
        #[cfg_attr(
            not(feature = "rust-guests"),
            expect(
                unused_variables,
                reason = "only a module that carries its application is held to the application's limit"
            )
        )]
        application: Option<isoplane_abi::config::GuestMemoryBytes>,
    ) -> Option<isoplane_abi::config::GuestMemoryBytes> {
        match self {
            Self::SharedRuntime { .. } => Some(isoplane_abi::config::GuestMemoryBytes::WASM32_MAX),
            #[cfg(feature = "rust-guests")]
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
