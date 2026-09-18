//! Application lifecycle, local resource budgets and initialization inputs.

use serde::{Deserialize, Serialize};

use super::Limits;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManifest {
    pub name: String,
    #[serde(default)]
    pub handlers: HandlerConfig,
    #[serde(default)]
    pub bindings: Bindings,
    #[serde(default)]
    pub limits: Limits,
    /// Operator-selected memory ceiling; other numeric budgets remain unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_memory_bytes: Option<super::GuestMemoryBytes>,
    /// Operator-selected resource budgets applied to the numeric defaults.
    #[serde(default)]
    pub resource_limits: super::ResourceLimitOverrides,
    /// Structural module bounds enforced before preparation.
    #[serde(default)]
    pub validation_limits: super::ValidationLimits,
    /// Non-secret variables handed to the init sandbox as a data blob.
    #[serde(default)]
    pub init_visible_vars: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandlerConfig {
    #[serde(default)]
    pub lifecycle: HandlerLifecycle,
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

/// Per-app: whether an instance may serve more than one dispatch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandlerLifecycle {
    /// Instances may survive across dispatches, and the platform resets or
    /// discards them whenever it chooses.
    #[default]
    Reuse,
    /// Every dispatch runs on a pristine clone of the template.
    Fresh,
}

impl HandlerLifecycle {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bindings {
    /// Egress allowlist. `None` leaves the platform's default egress policy in force.
    #[serde(default)]
    pub fetch_allowlist: Option<Vec<String>>,
}




impl AppManifest {
    /// Resource ceilings after applying operator overrides.
    pub fn effective_limits(&self) -> super::Limits {
        let mut limits = self.resource_limits.resolve(self.limits);
        limits.instance_memory_max = self.memory_max_bytes();
        limits
    }

    /// Memory ceiling after applying the independent operator override.
    pub fn memory_max_bytes(&self) -> u64 {
        self.guest_memory_bytes.map_or(
            self.limits.instance_memory_max,
            super::GuestMemoryBytes::get,
        )
    }
}
