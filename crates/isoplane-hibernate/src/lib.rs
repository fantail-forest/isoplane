//! Same-process pressure snapshots with explicitly reserved local backing.
//!
//! Guest memory and mutable state are saved against the immutable template.
//! Host resources remain owned by the original execution context during sleep.

#![forbid(unsafe_code)]

/// Snapshot operation result, retaining underlying storage errors.
pub type Result<T> = anyhow::Result<T>;

pub mod local;
pub mod snapshot;

/// The page unit shared with the guest/host contract.
pub use isoplane_abi::PAGE_SIZE as PAGE;

/// The module and ABI revision a saved local snapshot belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Original module identity.
    pub module_hash: isoplane_engine::ModuleHash,
    /// Original host interface revision.
    pub abi_rev: u32,
}

impl Binding {
    /// Names the module and host interface required for restoration.
    pub fn new(module_hash: isoplane_engine::ModuleHash, abi_rev: u32) -> Self {
        Self {
            module_hash,
            abi_rev,
        }
    }
}

/// Why a local snapshot cannot be restored. The execution owner decides
/// whether its dispatch must fail or an idle activation can start afresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeFallback {
    ModuleMismatch,
    AbiMismatch,
    /// A checksum failed, or the bytes were truncated or malformed.
    Corrupt,
    NotFound,
}

impl WakeFallback {
    /// Stable label for failed local snapshot reads.
    pub fn reason(self) -> &'static str {
        match self {
            WakeFallback::ModuleMismatch => "module_mismatch",
            WakeFallback::AbiMismatch => "abi_mismatch",
            WakeFallback::Corrupt => "corrupt",
            WakeFallback::NotFound => "not_found",
        }
    }
}

impl std::fmt::Display for WakeFallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "delta unusable: {}", self.reason())
    }
}

/// `Error` so a fallback can travel through an `anyhow::Result` without
/// losing its identity.
impl std::error::Error for WakeFallback {}

pub fn wake_error(f: WakeFallback) -> anyhow::Error {
    anyhow::Error::new(f)
}
