//! Snapshot identity and page-size primitives.

#![forbid(unsafe_code)]

/// Snapshot operation result, preserving the underlying error.
pub type Result<T> = anyhow::Result<T>;

pub mod delta;

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
