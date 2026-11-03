//! Application lifecycle, local resource budgets and initialization inputs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppManifest {
    pub name: String,
    #[serde(default)]
    pub bindings: Bindings,
    /// Operator-selected soft memory ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_memory_bytes: Option<super::GuestMemoryBytes>,
    /// Operator-selected resource budgets, resolved against the defaults.
    #[serde(default)]
    pub resource_limits: super::ResourceLimitOverrides,
    /// Structural module bounds enforced before preparation.
    #[serde(default)]
    pub validation_limits: super::ValidationLimits,
    /// Non-secret variables handed to the init sandbox as a data blob.
    #[serde(default)]
    pub init_visible_vars: Vec<(String, String)>,
}

/// Selects additional runtime initialization imports.
/// Application entropy admission is independent of this selector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InitSandbox {
    /// Uses the default initialization capability policy.
    #[default]
    Sealed,
    /// Permits additional runtime imports when the host records no active initialization unit.
    #[serde(rename = "runtime-bringup", alias = "runtime-bring-up")]
    RuntimeBringUp,
}

impl InitSandbox {
    pub fn as_str(self) -> &'static str {
        match self {
            InitSandbox::Sealed => "sealed",
            InitSandbox::RuntimeBringUp => "runtime-bringup",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    /// Egress allowlist. `None` leaves the platform's default egress policy in force.
    #[serde(default)]
    pub fetch_allowlist: Option<Vec<String>>,
}

impl AppManifest {
    /// Resource ceilings after applying operator overrides.
    pub fn effective_limits(&self) -> super::Limits {
        let mut limits = self.resource_limits.resolve();
        limits.instance_memory_max = self.memory_max_bytes();
        limits
    }

    /// Memory ceiling after applying the independent operator override.
    pub fn memory_max_bytes(&self) -> u64 {
        self.memory_limit().get()
    }

    /// Validated soft linear-memory allowance for initialization and execution.
    pub fn memory_limit(&self) -> super::GuestMemoryBytes {
        self.guest_memory_bytes.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::AppManifest;

    #[test]
    fn defaults_and_numeric_overrides_define_the_application_budgets() {
        let defaults: AppManifest = serde_json::from_str(r#"{"name":"app"}"#).unwrap();
        let limits = defaults.effective_limits();
        assert_eq!(limits.instances_per_application, 16);
        assert_eq!(limits.instance_memory_max, 512 << 20);
        assert_eq!(limits.occupancy_ms_per_dispatch, 300_000);
        assert_eq!(limits.pending_futures_per_dispatch, 4096);
        assert_eq!(limits.instance_open_connections, 32);
        assert_eq!(limits.package_bytes, 50 << 20);
        assert_eq!(limits.module_bytes_compressed, 50 << 20);

        let configured: AppManifest = serde_json::from_str(
            r#"{
            "name":"bounded", "guest_memory_bytes":67108864,
            "resource_limits": {"instances_per_application":0,
                "occupancy_ms_per_dispatch":17,"pending_futures_per_dispatch":3,
                "instance_open_connections":4,"package_bytes":1024}
        }"#,
        )
        .unwrap();
        let limits = configured.effective_limits();
        assert_eq!(limits.instances_per_application, 0);
        assert_eq!(limits.instance_memory_max, 64 << 20);
        assert_eq!(limits.occupancy_ms_per_dispatch, 17);
        assert_eq!(limits.pending_futures_per_dispatch, 3);
        assert_eq!(limits.instance_open_connections, 4);
        assert_eq!(limits.package_bytes, 1024);
        assert_eq!(limits.module_bytes_compressed, 50 << 20);
    }

}
