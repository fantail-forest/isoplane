//! Local module validation and preparation primitives.

/// WASI PAL definitions used by local preparation.
pub mod pal;
/// Runs local deploy-time initialization.
#[cfg(feature = "preinit")]
pub mod preinit;
pub mod validator;

pub use pal::{Delegation, Disposition, PalDef, PalSource, PAL_IMPORTS};
#[cfg(feature = "preinit")]
pub use preinit::{InitPal, PreInitOutcome, PreInitRequest, PreInitializer};
pub use validator::{validate, ValidationReport, Violation, ViolationCode};

pub type Result<T> = anyhow::Result<T>;
