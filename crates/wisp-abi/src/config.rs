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
    /// §4.4: non-secret vars the tenant marks init-visible, supplied to the
    /// sealed init sandbox as a data blob. Runtime secrets never appear here.
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
    /// §8.6 egress policy. RFC1918 / link-local are denied by default.
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
            cpu_ms_per_http_dispatch: 50,
            subrequests_per_dispatch: 200,
            pending_futures_per_dispatch: 64,
        }
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
