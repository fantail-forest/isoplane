//! Measured preparation stages shared with the local execution provider.

mod deploy;

/// Observed stages returned by local preparation.
pub use deploy::DeployStage;
