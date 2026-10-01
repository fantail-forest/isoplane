//! Initialization limits and installed runtime selection.

use serde::{Deserialize, Serialize};

/// Operator-controlled initialization policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationConfig {
    /// Initialization occupancy budget across replay attempts.
    pub initialization_occupancy_ms: u32,
    /// Total initialization wall deadline, including waits and replay passes.
    pub initialization_wall_ms: u32,
    /// Maximum impurity-deferral replay attempts.
    pub initialization_passes: u32,
    /// Maximum retained successful initialization log bytes.
    pub initialization_log_bytes: usize,
    /// The shared runtimes installed with this node, one directory per SDK
    /// release beside the `inventory.json` listing them. Application packages
    /// are prepared only on these; a node without it prepares no package.
    #[serde(default)]
    pub runtimes: Option<std::path::PathBuf>,
}

impl PreparationConfig {
    /// Refuse invalid initialization limits.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.initialization_occupancy_ms > 0
                && self.initialization_wall_ms > 0
                && self.initialization_passes > 0
                && self.initialization_log_bytes > 0,
            "preparation initialization limits must be positive"
        );
        Ok(())
    }

}
