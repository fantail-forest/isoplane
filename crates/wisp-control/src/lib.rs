//! Local module validation and preparation primitives.

pub mod aot;
#[cfg(feature = "preinit")]
pub mod preinit;
pub mod validator;

#[cfg(feature = "preinit")]
pub use preinit::{PreInitError, PreInitOutcome, PreInitRequest, PreInitializer};
pub use validator::{validate, ValidationReport, Violation, ViolationCode};
