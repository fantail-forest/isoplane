//! Numeric application resource budgets.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub module_bytes_compressed: u64,
    pub instance_memory_max: u64,
    pub cpu_ms_per_http_dispatch: u32,
    pub subrequests_per_dispatch: u32,
    pub pending_futures_per_dispatch: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            module_bytes_compressed: 20971520,
            instance_memory_max: 134217728,
            cpu_ms_per_http_dispatch: 30000,
            subrequests_per_dispatch: 200,
            pending_futures_per_dispatch: 64,
        }
    }
}
