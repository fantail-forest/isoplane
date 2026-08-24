//! Local module validation and preparation primitives.

pub mod preinit;
pub mod validator;

pub use preinit::{PreInitOutcome, PreInitRequest, PreInitializer};
pub use validator::{validate, ValidationReport, Violation, ViolationCode};

pub type Result<T> = anyhow::Result<T>;
