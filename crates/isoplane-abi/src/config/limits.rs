//! Numeric application resource budgets.

use serde::{Deserialize, Serialize};

/// Effective per-application resource budgets.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Maximum zstd-compressed module bytes at admission.
    pub module_bytes_compressed: u64,
    /// Maximum size of an application package: the uncompressed bytes of
    /// every file it holds.
    pub package_bytes: u64,
    /// Maximum guest linear-memory bytes.
    pub instance_memory_max: u64,
    /// Occupied-worker milliseconds across all slices of one dispatch.
    pub occupancy_ms_per_http_dispatch: u32,
    /// Maximum subrequests started within one dispatch.
    pub subrequests_per_dispatch: u32,
    /// Maximum concurrently pending futures within one dispatch.
    pub pending_futures_per_dispatch: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            module_bytes_compressed: 52428800,
            package_bytes: 52428800,
            instance_memory_max: 134217728,
            occupancy_ms_per_http_dispatch: 30000,
            subrequests_per_dispatch: 200,
            pending_futures_per_dispatch: 64,
        }
    }
}

/// Operator overrides for the resource budgets enforced for an application.
/// Unspecified values retain the supplied numeric defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimitOverrides {
    /// Maximum compressed module size admitted for preparation.
    pub module_bytes_compressed: Option<u64>,
    /// Maximum uncompressed application package size admitted for preparation.
    pub package_bytes: Option<u64>,
    /// occupancy budget across all invocation slices of one HTTP dispatch.
    pub occupancy_ms_per_http_dispatch: Option<u32>,
    /// Maximum subrequests started by one dispatch.
    pub subrequests_per_dispatch: Option<u32>,
    /// Maximum pending futures owned by one dispatch.
    pub pending_futures_per_dispatch: Option<u32>,
}

impl ResourceLimitOverrides {
    /// Apply explicit overrides to the supplied numeric budgets.
    pub fn resolve(self, defaults: Limits) -> Limits {
        Limits {
            module_bytes_compressed: self
                .module_bytes_compressed
                .unwrap_or(defaults.module_bytes_compressed),
            package_bytes: self.package_bytes.unwrap_or(defaults.package_bytes),
            occupancy_ms_per_http_dispatch: self
                .occupancy_ms_per_http_dispatch
                .unwrap_or(defaults.occupancy_ms_per_http_dispatch),
            subrequests_per_dispatch: self
                .subrequests_per_dispatch
                .unwrap_or(defaults.subrequests_per_dispatch),
            pending_futures_per_dispatch: self
                .pending_futures_per_dispatch
                .unwrap_or(defaults.pending_futures_per_dispatch),
            ..defaults
        }
    }
}
