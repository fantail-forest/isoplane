//! Trusted producer provenance, independent of adjustable tenant quotas.

use serde::{Deserialize, Serialize};

/// Provenance supplied by the trusted build pipeline, never inferred from client WASM.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeProfile {
    /// Raw WASM admission makes no managed-runtime or hosted IL claim.
    RawWasm,
    /// The shared runtime a node installs for one SDK release, which
    /// application packages built with that release run on.
    SharedRuntime {
        /// The SDK release that carries the runtime.
        sdk_version: String,
    },
    /// A .NET 10 toolchain with content identities supplied by its trusted publisher.
    DotNet10 {
        /// Selected managed execution mode.
        mode: ManagedMode,
        /// Content identities for the complete installed runtime and build closure.
        components: Box<ManagedComponents>,
    },
}

/// Managed execution modes retain distinct build provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedMode {
    /// Ahead-of-time compilation of customer IL to portable WASM.
    NativeAot,
    /// Mono interpretation of customer IL inside the WASM sandbox.
    Mono,
}

/// Digests of trusted release artifacts; version strings alone do not establish identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedComponents {
    /// Exact .NET SDK content used by the producer.
    pub sdk: [u8; 32],
    /// Exact guest CLR runtime and its patches.
    pub runtime: [u8; 32],
    /// Base-class and framework assembly closure.
    pub libraries: [u8; 32],
    /// The selected IL compiler or interpreter build tool.
    pub compiler: [u8; 32],
    /// Platform adapter and export/scheduler transformations.
    pub platform: [u8; 32],
    /// WASI SDK and libc artifact closure.
    pub wasi: [u8; 32],
    /// Managed/native layout contract and applied adaptation.
    pub native_layout: [u8; 32],
}

impl RuntimeProfile {
    /// Profile for explicitly raw WASM, without invented managed provenance.
    pub fn raw_wasm() -> Self {
        Self::RawWasm
    }

    /// The memory maximum a module of this profile may declare, given its
    /// application's limit: that limit, except for a shared runtime, which
    /// serves applications with different limits and is held only to its
    /// plan's. Initialization and execution hold every instance to its
    /// application's limit either way.
    pub fn declared_memory_ceiling(
        &self,
        application: Option<isoplane_abi::config::GuestMemoryBytes>,
    ) -> Option<isoplane_abi::config::GuestMemoryBytes> {
        match self {
            Self::SharedRuntime { .. } => None,
            Self::RawWasm | Self::DotNet10 { .. } => application,
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
