//! Local module validation and preparation primitives.

/// Runs local deploy-time initialization.
#[cfg(feature = "preinit")]
pub mod preinit;
pub mod validator;

#[cfg(feature = "preinit")]
pub use preinit::{InitPal, PreInitOutcome, PreInitRequest, PreInitializer};
pub use validator::{validate, ValidationReport, Violation, ViolationCode};

pub type Result<T> = anyhow::Result<T>;
