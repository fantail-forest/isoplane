//! Local module validation and preparation primitives.

#![forbid(unsafe_code)]

pub mod aot;
#[cfg(feature = "preinit")]
pub use isoplane_prepare::preinit;
pub use isoplane_runtime::validator;

#[cfg(feature = "preinit")]
pub use preinit::{PreInitError, PreInitOutcome, PreInitRequest, PreInitializer};
pub use validator::{validate, ValidationReport, Violation, ViolationCode};
