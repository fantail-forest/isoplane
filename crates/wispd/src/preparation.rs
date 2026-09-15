//! Initialization limits and installed runtime selection.

use serde::{Deserialize, Serialize};

/// Operator-controlled initialization policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationConfig {
    /// Initialization CPU budget across replay attempts.
    pub initialization_cpu_ms: u32,
    /// Total initialization wall deadline, including waits and replay passes.
    pub initialization_wall_ms: u32,
    /// Maximum impurity-deferral replay attempts.
    pub initialization_passes: u32,
    /// Maximum retained successful initialization log bytes.
    pub initialization_log_bytes: usize,
}

impl PreparationConfig {
    /// Refuse invalid initialization limits.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.initialization_cpu_ms > 0
                && self.initialization_wall_ms > 0
                && self.initialization_passes > 0
                && self.initialization_log_bytes > 0,
            "preparation initialization limits must be positive"
        );
        Ok(())
    }


}
