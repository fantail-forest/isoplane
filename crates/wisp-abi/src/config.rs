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
    /// Non-secret vars the tenant marks init-visible, supplied to the sealed
    /// init sandbox as a data blob. Runtime secrets never appear here.
    #[serde(default)]
    pub init_visible_vars: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandlerConfig {
    #[serde(default)]
    pub lifecycle: HandlerLifecycle,
}

/// Policy for reusing guest state across handler dispatches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandlerLifecycle {
    /// Reuse guest memory opportunistically between dispatches.
    /// The host may reset or discard an instance; memory is temporary.
    #[default]
    Reuse,
    /// Use initialized guest state for each dispatch.
    Fresh,
}

impl HandlerLifecycle {
    /// Whether a reset after every dispatch is *guaranteed* rather than merely
    /// permitted.
    pub fn resets_every_dispatch(self) -> bool {
        self == HandlerLifecycle::Fresh
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bindings {
    /// Egress policy. RFC1918 / link-local are denied by default.
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
    /// Maximum pending futures allowed for a dispatch.
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

/// Execution and residency states reported by the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleTier {
    /// Executing on a pool thread.
    Hot,
    /// Empty invocation stack with host-owned wake sources and resident guest memory.
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

    /// Whether this state retains resident guest memory.
    pub fn is_resident(self) -> bool {
        matches!(self, LifecycleTier::Hot | LifecycleTier::Parked)
    }

    /// The legal tier transitions as a predicate, and nothing outside them.
    ///
    /// This lives with the tiers rather than in the cell because it *is* the
    /// table: `Hibernated -> Parked` is absent because the only exit from
    /// Hibernated is a wake trigger, which makes the instance Hot; and nothing
    /// leaves Discarded, because a fresh activation produces a *new* instance
    /// rather than resurrecting this one.
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
    fn fresh_guarantees_reset() {
        assert!(HandlerLifecycle::Fresh.resets_every_dispatch());
        assert!(!HandlerLifecycle::Reuse.resets_every_dispatch());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn only_hot_and_parked_hold_memory() {
        assert!(LifecycleTier::Hot.is_resident());
        assert!(LifecycleTier::Parked.is_resident());
        assert!(!LifecycleTier::Hibernated.is_resident());
        assert!(!LifecycleTier::Discarded.is_resident());
    }
}
