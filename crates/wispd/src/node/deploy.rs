//! Measured preparation stages shared with the local execution provider.

use serde::{Deserialize, Serialize};

/// One measured preparation stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployStage {
    /// Stage name.
    pub name: String,
    /// Observed elapsed milliseconds.
    pub ms: f64,
}
