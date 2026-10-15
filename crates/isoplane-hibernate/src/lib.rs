//! Same-process pressure snapshots with reserved local backing.
//! Host resources remain owned by the original execution context during sleep.

#![forbid(unsafe_code)]

/// Snapshot operation result, preserving the underlying error.
pub type Result<T> = anyhow::Result<T>;

pub mod delta;

pub mod local;

pub mod snapshot;

pub use delta::{Binding, PAGE};

/// Why saved application state cannot be restored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeFallback {
    ModuleMismatch,
    AbiMismatch,
    /// A checksum failed, or bytes were truncated or malformed.
    Corrupt,
    NotFound,
}

impl WakeFallback {
    /// Stable label for a failed snapshot read.
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

impl std::error::Error for WakeFallback {}

/// Wraps a restore refusal while retaining its typed reason.
pub fn wake_error(f: WakeFallback) -> anyhow::Error {
    anyhow::Error::new(f)
}
