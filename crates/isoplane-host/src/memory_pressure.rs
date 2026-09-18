//! Host memory headroom, bounded by the process's visible resource hierarchy.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod linux;

/// A quantity of host memory in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemoryBytes(pub u64);

/// Available memory and capacity after applying host and container limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySample {
    /// Headroom before either the host or a visible cgroup exhausts memory.
    pub available: MemoryBytes,
    /// The smallest host or visible cgroup memory capacity.
    pub total: MemoryBytes,
}

/// Failure to obtain a trustworthy memory sample.
#[derive(Debug, thiserror::Error)]
pub enum MemoryPressureError {
    /// A required kernel interface could not be read.
    #[error("cannot read memory pressure interface {path}: {source}")]
    Read {
        /// Interface whose read failed.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// Kernel data could not be interpreted safely.
    #[error("invalid memory pressure interface {path}: {reason}")]
    Invalid {
        /// Interface containing invalid data.
        path: PathBuf,
        /// Interpretation that failed.
        reason: &'static str,
    },
    /// The host's memory accounting interface is unsupported.
    #[error("memory pressure sampling is unsupported: {0}")]
    Unsupported(&'static str),
}

/// Supplies host-wide memory pressure without exposing platform mechanisms.
pub trait MemoryPressureSource: Send + Sync {
    /// Reads a fresh sample, returning an error rather than invented headroom.
    fn sample(&self) -> Result<MemorySample, MemoryPressureError>;
}

/// Samples the running host and the process's effective container limits.
#[derive(Debug, Default)]
pub struct SystemMemoryPressure;

impl MemoryPressureSource for SystemMemoryPressure {
    fn sample(&self) -> Result<MemorySample, MemoryPressureError> {
        #[cfg(target_os = "linux")]
        {
            linux::sample(&|path| std::fs::read_to_string(path))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(MemoryPressureError::Unsupported(
                "requires Linux procfs and cgroup v2",
            ))
        }
    }
}
