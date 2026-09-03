//! Application lifecycle, local resource budgets and initialization inputs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManifest {
    pub name: String,
    #[serde(default)]
    pub handlers: HandlerConfig,
    #[serde(default)]
    pub bindings: Bindings,
    #[serde(default)]
    pub limits: Limits,
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

/// The ports a node listens on unless configured otherwise, shared with the
/// CLI's defaults.
pub const DEFAULT_PROXY_PORT: u16 = 8787;
pub const DEFAULT_ADMIN_PORT: u16 = 8788;

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

/// The lifecycle tiers an instance moves through. Defined here because the
/// cell owns the transitions while `wispd` reports the counts and `wisp
/// status` prints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleTier {
    /// Executing on a pool thread.
    Hot,
    /// Empty stack, futures pending, memory resident.
    Parked,
    /// Resumable sandbox state is retained at a host return boundary.
    Hibernated,
    /// No resumable sandbox state is retained.
    Discarded,
}

impl LifecycleTier {
    pub const ALL: &'static [LifecycleTier] = &[
        LifecycleTier::Hot,
        LifecycleTier::Parked,
        LifecycleTier::Hibernated,
        LifecycleTier::Discarded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleTier::Hot => "hot",
            LifecycleTier::Parked => "parked",
            LifecycleTier::Hibernated => "hibernated",
            LifecycleTier::Discarded => "discarded",
        }
    }

    /// Whether an instance in this tier occupies linear memory. The proxy's
    /// wake coalescing and the cell's residency accounting both key off it.
    pub fn is_resident(self) -> bool {
        matches!(self, LifecycleTier::Hot | LifecycleTier::Parked)
    }

    /// The legal transitions. `Hibernated -> Parked` is absent because the
    /// only exit from `Hibernated` is a wake, which makes the instance `Hot`;
    /// nothing leaves `Discarded` because a fresh activation is a new instance.
    pub fn may_transition_to(self, next: Self) -> bool {
        use LifecycleTier::*;
        matches!(
            (self, next),
            (Hot, Parked)
                | (Hot, Discarded)
                | (Parked, Hot)
                | (Parked, Hibernated)
                | (Parked, Discarded)
                | (Hibernated, Hot)
        )
    }
}

impl std::fmt::Display for LifecycleTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hot_and_parked_hold_memory() {
        assert!(
            LifecycleTier::Hot.is_resident(),
            "a hot instance is in memory"
        );
        assert!(
            LifecycleTier::Parked.is_resident(),
            "a parked instance is in memory"
        );
        assert!(
            !LifecycleTier::Hibernated.is_resident(),
            "a hibernated instance is bytes in storage"
        );
        assert!(
            !LifecycleTier::Discarded.is_resident(),
            "a discarded instance holds nothing"
        );
    }
}
