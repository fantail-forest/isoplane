//! Node configuration and startup validation.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_abi::host::CodeTier;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    #[serde(default = "default_node_name")]
    pub node: String,
    #[serde(default)]
    pub proxy: ProxyListen,
    #[serde(default)]
    pub admin: AdminListen,
    #[serde(default)]
    pub cells: Vec<CellConfig>,
    #[serde(default)]
    pub apps: Vec<AppConfig>,
    /// Directory for local node data.
    #[serde(default = "default_data_dir")]
    pub data_dir: std::path::PathBuf,
    #[serde(default)]
    pub engine: EngineLimits,
}

/// The engine sizing a real node's cells share (§6.2, §11.1).
///
/// These are a node-level setting rather than a constant because they are not
/// free: the pooling allocator reserves `max_instances × (max_memory + guard)`
/// of virtual address space, which at the defaults is ~21 TiB per engine.
/// That costs no physical memory, but a 47-bit address space holds about six
/// of them — so a machine running several nodes, or a test binary building
/// several, has to be able to say "smaller". Leaving this unconfigurable is
/// how you get a node that works alone and fails in a fleet.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EngineLimits {
    /// Ceiling on concurrently resident instances per engine. G1 wants ≥ 5,000
    /// (§6.2), so the default leaves headroom above it.
    #[serde(default = "default_max_instances")]
    pub max_instances: u32,
    /// Per-instance linear-memory ceiling (§5.5, §11.1).
    #[serde(default = "default_max_memory_mib")]
    pub max_memory_mib: u64,
    /// §8.2.1's reserved guard region, which is what lets Cranelift elide
    /// explicit bounds checks.
    #[serde(default = "default_guard_mib")]
    pub guard_mib: u64,
}

fn default_max_instances() -> u32 {
    10_000
}
fn default_max_memory_mib() -> u64 {
    128
}
fn default_guard_mib() -> u64 {
    2048
}

impl Default for EngineLimits {
    fn default() -> Self {
        EngineLimits {
            max_instances: default_max_instances(),
            max_memory_mib: default_max_memory_mib(),
            guard_mib: default_guard_mib(),
        }
    }
}

fn default_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("./wispd-data")
}

fn default_node_name() -> String {
    "wispd-0".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyListen {
    pub listen: SocketAddr,
    #[serde(default = "default_max_in_flight")]
    pub max_in_flight: usize,
    #[serde(default = "default_max_queued")]
    pub max_queued: usize,
}

fn default_max_in_flight() -> usize {
    256
}
fn default_max_queued() -> usize {
    1024
}

impl Default for ProxyListen {
    fn default() -> Self {
        ProxyListen {
            listen: "127.0.0.1:8787".parse().unwrap(),
            max_in_flight: default_max_in_flight(),
            max_queued: default_max_queued(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminListen {
    pub listen: SocketAddr,
}

impl Default for AdminListen {
    fn default() -> Self {
        AdminListen {
            listen: "127.0.0.1:8788".parse().unwrap(),
        }
    }
}

/// A named execution cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellConfig {
    pub name: String,
    #[serde(default = "default_max_hot")]
    pub max_hot: usize,
}

fn default_max_hot() -> usize {
    32
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub name: String,
    pub tenant: String,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub lifecycle: HandlerLifecycle,
    /// Cell selected for this application's instances.
    pub cell: String,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default = "default_path_prefix")]
    pub path_prefix: String,
    #[serde(default)]
    pub versions: Vec<VersionConfig>,
    /// §4.4 / §9.5: non-secret vars the tenant marks `init-visible`, handed to
    /// the sealed init sandbox as a data blob so config parsing happens once at
    /// deploy time instead of once per activation. Runtime secrets are declared
    /// by name elsewhere and are never available here (§8.6).
    #[serde(default)]
    pub init_visible_vars: BTreeMap<String, String>,
    /// Additional runtime initialization capability policy.
    /// Application entropy is governed separately by the preparation context.
    #[serde(default)]
    pub init_sandbox: InitSandbox,
}

fn default_path_prefix() -> String {
    "/".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionConfig {
    pub id: String,
    #[serde(default)]
    pub tier: TierConfig,
    /// Local module path. Requests fail when no module is installed.
    #[serde(default)]
    pub wasm: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TierConfig {
    /// Fast compilation tier.
    #[default]
    Fast,
    Optimized,
}

impl From<TierConfig> for CodeTier {
    fn from(t: TierConfig) -> Self {
        match t {
            TierConfig::Fast => CodeTier::Fast,
            TierConfig::Optimized => CodeTier::Optimized,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("no cells configured: a node needs at least one")]
    NoCells,
    #[error("duplicate cell name `{0}`")]
    DuplicateCell(String),
    #[error("app `{app}` is placed on cell `{cell}`, which does not exist")]
    NoSuchCell { app: String, cell: String },
    #[error("app `{0}` declares no versions")]
    NoVersions(String),
    #[error("duplicate app name `{0}`")]
    DuplicateApp(String),
    #[error("proxy.max_in_flight must be at least 1 (got {0}): a node that admits nothing is not a node")]
    ZeroInFlight(usize),
    #[error("cell `{cell}` has max_hot = 0")]
    ZeroMaxHot { cell: String },
}

impl NodeConfig {
    pub fn from_toml(s: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(s)?)
    }

    /// Two execution cells and one demonstration application.
    pub fn dev_default() -> Self {
        NodeConfig {
            node: "wisp-dev".into(),
            proxy: ProxyListen {
                listen: "127.0.0.1:8787".parse().unwrap(),
                ..Default::default()
            },
            admin: AdminListen::default(),
            cells: vec![
                CellConfig {
                    name: "cell-0".into(),
                    max_hot: 16,
                },
                CellConfig {
                    name: "cell-1".into(),
                    max_hot: 32,
                },
            ],
            apps: vec![AppConfig {
                name: "quotes".into(),
                tenant: "acme".into(),
                limits: Limits::default(),
                lifecycle: HandlerLifecycle::Reuse,
                cell: "cell-1".into(),
                hosts: vec![
                    "quotes.wisp.dev".into(),
                    "localhost".into(),
                    "127.0.0.1".into(),
                ],
                path_prefix: "/".into(),
                versions: vec![VersionConfig {
                    id: "v1".into(),
                    tier: TierConfig::Fast,
                    wasm: None,
                }],
                init_visible_vars: BTreeMap::new(),
                init_sandbox: InitSandbox::Sealed,
            }],
            data_dir: default_data_dir(),
            engine: EngineLimits::default(),
        }
    }

    /// The startup gate. Everything here is a refusal, never a warning.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.cells.is_empty() {
            return Err(ConfigError::NoCells);
        }
        if self.proxy.max_in_flight == 0 {
            return Err(ConfigError::ZeroInFlight(self.proxy.max_in_flight));
        }
        if let Some(c) = self.cells.iter().find(|c| c.max_hot == 0) {
            return Err(ConfigError::ZeroMaxHot {
                cell: c.name.clone(),
            });
        }
        let mut seen = BTreeSet::new();
        for c in &self.cells {
            if !seen.insert(c.name.clone()) {
                return Err(ConfigError::DuplicateCell(c.name.clone()));
            }
        }
        let mut seen_apps = BTreeSet::new();
        for a in &self.apps {
            if !seen_apps.insert(a.name.clone()) {
                return Err(ConfigError::DuplicateApp(a.name.clone()));
            }
        }

        let configured_cells: std::collections::BTreeSet<&str> =
            self.cells.iter().map(|cell| cell.name.as_str()).collect();

        // Which apps landed on which cell, in declaration order so the error
        // names the first offending pair rather than an arbitrary one.
        for app in &self.apps {
            if !configured_cells.contains(app.cell.as_str()) {
                return Err(ConfigError::NoSuchCell {
                    app: app.name.clone(),
                    cell: app.cell.clone(),
                });
            }
        }

        for app in &self.apps {
            if app.versions.is_empty() {
                return Err(ConfigError::NoVersions(app.name.clone()));
            }
        }
        Ok(())
    }

    pub fn cell(&self, name: &str) -> Option<&CellConfig> {
        self.cells.iter().find(|c| c.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(toml_str: &str) -> NodeConfig {
        NodeConfig::from_toml(toml_str).expect("valid toml")
    }

    const TWO_CELLS: &str = r#"
node = "n1"
[[cells]]
name = "cell-0"
[[cells]]
name = "cell-1"
"#;

    #[test]
    fn the_dev_default_is_valid() {
        NodeConfig::dev_default().validate().unwrap();
    }

    #[test]
    fn applications_can_share_a_configured_cell() {
        let c = cfg(&format!(
            r#"{TWO_CELLS}
[[apps]]
name = "a"
tenant = "t1"
limits.module_bytes_compressed = 20971520
limits.instance_memory_max = 134217728
limits.cpu_ms_per_http_dispatch = 30000
limits.subrequests_per_dispatch = 200
limits.pending_futures_per_dispatch = 64
cell = "cell-1"
versions = [{{ id = "v1" }}]

[[apps]]
name = "b"
tenant = "t2"
limits.module_bytes_compressed = 20971520
limits.instance_memory_max = 134217728
limits.cpu_ms_per_http_dispatch = 30000
limits.subrequests_per_dispatch = 200
limits.pending_futures_per_dispatch = 64
cell = "cell-1"
versions = [{{ id = "v1" }}]
"#
        ));
        c.validate().unwrap();
    }

    #[test]
    fn one_owner_can_configure_several_applications() {
        let c = cfg(r#"
[[cells]]
name = "cell-2"

[[apps]]
name = "a"
tenant = "bank"
limits.module_bytes_compressed = 52428800
limits.instance_memory_max = 536870912
limits.cpu_ms_per_http_dispatch = 300000
limits.subrequests_per_dispatch = 10000
limits.pending_futures_per_dispatch = 4096
cell = "cell-2"
versions = [{ id = "v1" }]

[[apps]]
name = "b"
tenant = "bank"
limits.module_bytes_compressed = 52428800
limits.instance_memory_max = 536870912
limits.cpu_ms_per_http_dispatch = 300000
limits.subrequests_per_dispatch = 10000
limits.pending_futures_per_dispatch = 4096
cell = "cell-2"
versions = [{ id = "v1" }]
"#);
        c.validate().unwrap();
    }

    #[test]
    fn placement_onto_a_nonexistent_cell_is_refused() {
        let c = cfg(&format!(
            r#"{TWO_CELLS}
[[apps]]
name = "a"
tenant = "t"
limits.module_bytes_compressed = 20971520
limits.instance_memory_max = 134217728
limits.cpu_ms_per_http_dispatch = 30000
limits.subrequests_per_dispatch = 200
limits.pending_futures_per_dispatch = 64
cell = "nope"
versions = [{{ id = "v1" }}]
"#
        ));
        assert_eq!(
            c.validate(),
            Err(ConfigError::NoSuchCell {
                app: "a".into(),
                cell: "nope".into()
            })
        );
    }

    #[test]
    fn a_node_with_no_cells_is_refused() {
        assert_eq!(cfg("node = \"n\"").validate(), Err(ConfigError::NoCells));
    }

    #[test]
    fn degenerate_capacity_settings_are_refused_at_startup() {
        let mut c = NodeConfig::dev_default();
        c.proxy.max_in_flight = 0;
        assert_eq!(c.validate(), Err(ConfigError::ZeroInFlight(0)));

        let mut c = NodeConfig::dev_default();
        c.cells[0].max_hot = 0;
        assert_eq!(
            c.validate(),
            Err(ConfigError::ZeroMaxHot {
                cell: "cell-0".into()
            })
        );
    }

    #[test]
    fn duplicate_cell_names_are_refused() {
        let c = cfg(r#"
[[cells]]
name = "c"
[[cells]]
name = "c"
"#);
        assert_eq!(c.validate(), Err(ConfigError::DuplicateCell("c".into())));
    }
}
