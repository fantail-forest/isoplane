//! Numeric application resource budgets.

use serde::{Deserialize, Serialize};

/// Default maximum outbound connections held by one instance.
pub const DEFAULT_INSTANCE_OPEN_CONNECTIONS: u64 = 32;

/// Effective per-application resource budgets.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    /// Maximum zstd-compressed module bytes at admission.
    pub module_bytes_compressed: u64,
    /// Maximum size of an application package: the uncompressed bytes of
    /// every file it holds.
    pub package_bytes: u64,
    /// Maximum live instances of an application, including held and draining instances.
    pub instances_per_application: u32,
    /// Maximum guest linear-memory bytes.
    pub instance_memory_max: u64,
    /// Occupied-worker milliseconds across all slices of one dispatch.
    pub occupancy_ms_per_dispatch: u32,
    /// Maximum outbound connections one instance may hold, including attempts in progress.
    pub instance_open_connections: u64,
    /// Maximum concurrently pending futures within one dispatch.
    pub pending_futures_per_dispatch: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            instances_per_application: 8,
            module_bytes_compressed: 52428800,
            package_bytes: 52428800,
            instance_memory_max: 134217728,
            occupancy_ms_per_dispatch: 30000,
            instance_open_connections: 32,
            pending_futures_per_dispatch: 64,
        }
    }
}

/// Operator overrides for the resource budgets enforced for an application.
/// Unspecified values retain the supplied numeric defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimitOverrides {
    /// Maximum live application instances; zero refuses new activations.
    pub instances_per_application: Option<u32>,
    /// Maximum compressed module size admitted for preparation.
    pub module_bytes_compressed: Option<u64>,
    /// Maximum uncompressed application package size admitted for preparation.
    pub package_bytes: Option<u64>,
    /// Occupancy budget across all slices of one request dispatch.
    pub occupancy_ms_per_dispatch: Option<u32>,
    /// Maximum outbound connections one instance may hold concurrently.
    pub instance_open_connections: Option<u64>,
    /// Maximum pending futures owned by one dispatch.
    pub pending_futures_per_dispatch: Option<u32>,
}

impl ResourceLimitOverrides {
    /// Apply explicit overrides to the supplied numeric budgets.
    pub fn resolve(self, defaults: Limits) -> Limits {
        Limits {
            instances_per_application: self
                .instances_per_application
                .unwrap_or(defaults.instances_per_application),
            module_bytes_compressed: self
                .module_bytes_compressed
                .unwrap_or(defaults.module_bytes_compressed),
            package_bytes: self.package_bytes.unwrap_or(defaults.package_bytes),
            occupancy_ms_per_dispatch: self
                .occupancy_ms_per_dispatch
                .unwrap_or(defaults.occupancy_ms_per_dispatch),
            instance_open_connections: self
                .instance_open_connections
                .unwrap_or(defaults.instance_open_connections),
            pending_futures_per_dispatch: self
                .pending_futures_per_dispatch
                .unwrap_or(defaults.pending_futures_per_dispatch),
            ..defaults
        }
    }
}
