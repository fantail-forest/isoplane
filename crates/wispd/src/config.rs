//! Node configuration and startup validation.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use wisp_abi::config::{
    HandlerLifecycle, InitSandbox, Limits, DEFAULT_ADMIN_PORT, DEFAULT_PROXY_PORT,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Directory for local node artifacts.
    #[serde(default = "default_data_dir")]
    pub data_dir: std::path::PathBuf,
    #[serde(default)]
    pub engine: EngineLimits,
    /// High-water mark for each real cell's queued HTTP response bytes.
    #[serde(default = "default_response_queue_bytes")]
    pub response_queue_bytes: usize,
    /// Full OTLP HTTP/protobuf endpoint for structured dispatch events. Unset disables export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_endpoint: Option<crate::logs::OtlpEndpoint>,
    /// The public TLS root bundle guests read at
    /// `/etc/ssl/certs/ca-certificates.crt`.
    #[serde(default)]
    pub trust: crate::trust::TrustConfig,
    /// Initialization limits and installed runtime selection.
    #[cfg(feature = "real-backends")]
    #[serde(default)]
    pub preparation: Option<crate::preparation::PreparationConfig>,
}

fn default_response_queue_bytes() -> usize {
    wisp_abi::host::DEFAULT_RESPONSE_QUEUE_BYTES
}

/// The engine sizing a real node's cells share. The pooling allocator
/// reserves `max_instances × (max_memory + guard)` of address space up front,
/// so several nodes on one machine have to be able to say "smaller".
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineLimits {
    /// Ceiling on concurrently resident instances per engine.
    #[serde(default = "default_max_instances")]
    pub max_instances: u32,
    /// Per-instance linear-memory ceiling.
    #[serde(default = "default_max_memory_mib")]
    pub max_memory_mib: u64,
    /// The guard region after each memory, which lets Cranelift elide
    /// bounds checks. Address space, not memory.
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
    32
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
#[serde(deny_unknown_fields)]
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
            listen: SocketAddr::from(([127, 0, 0, 1], DEFAULT_PROXY_PORT)),
            max_in_flight: default_max_in_flight(),
            max_queued: default_max_queued(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminListen {
    pub listen: SocketAddr,
}

impl Default for AdminListen {
    fn default() -> Self {
        AdminListen {
            listen: SocketAddr::from(([127, 0, 0, 1], DEFAULT_ADMIN_PORT)),
        }
    }
}

/// A named execution cell.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellConfig {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    pub name: String,
    pub tenant: String,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub lifecycle: HandlerLifecycle,
    /// Which cell this app's instances run in.
    pub cell: String,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default = "default_path_prefix")]
    pub path_prefix: String,
    #[serde(default)]
    pub versions: Vec<VersionConfig>,
    /// Non-secret vars handed to the init sandbox as a data blob.
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
#[serde(deny_unknown_fields)]
pub struct VersionConfig {
    pub id: String,
    /// The module to deploy for this version at startup. `None` is routable
    /// with nothing to run: a request for it fails naming the missing
    /// template.
    #[serde(default)]
    pub wasm: Option<std::path::PathBuf>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid preparation configuration: {0}")]
    Preparation(String),
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
    #[error("response_queue_bytes must be positive")]
    ZeroResponseQueue,
    #[error(transparent)]
    Trust(#[from] crate::trust::TrustError),
}

impl NodeConfig {
    pub fn from_toml(s: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(s)?)
    }

    /// Two cells and one app.
    pub fn dev_default() -> Self {
        NodeConfig {
            node: "wisp-dev".into(),
            proxy: ProxyListen::default(),
            response_queue_bytes: default_response_queue_bytes(),
            otlp_logs_endpoint: None,
            admin: AdminListen::default(),
            cells: vec![
                CellConfig {
                    name: "cell-0".into(),
                },
                CellConfig {
                    name: "cell-1".into(),
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
                    wasm: None,
                }],
                init_visible_vars: BTreeMap::new(),
                init_sandbox: InitSandbox::Sealed,
            }],
            data_dir: default_data_dir(),
            engine: EngineLimits::default(),
            trust: crate::trust::TrustConfig::default(),
            #[cfg(feature = "real-backends")]
            preparation: None,
        }
    }

    /// The startup gate: a refusal, never a warning. Placement rules first,
    /// then the trust bundle, which is the one check that reads a file.
    pub fn validate(&self) -> Result<(), ConfigError> {
        #[cfg(feature = "real-backends")]
        if let Some(preparation) = &self.preparation {
            preparation
                .validate()
                .map_err(|error| ConfigError::Preparation(error.to_string()))?;
        }
        self.validate_placement()?;
        self.trust_bundle().map(drop)
    }

    /// The verified root bundle this node serves to guests.
    pub fn trust_bundle(&self) -> Result<crate::trust::TrustBundle, ConfigError> {
        Ok(crate::trust::TrustBundle::load(&self.trust)?)
    }

    fn validate_placement(&self) -> Result<(), ConfigError> {
        if self.cells.is_empty() {
            return Err(ConfigError::NoCells);
        }
        if self.response_queue_bytes == 0 {
            return Err(ConfigError::ZeroResponseQueue);
        }
        if self.proxy.max_in_flight == 0 {
            return Err(ConfigError::ZeroInFlight(self.proxy.max_in_flight));
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
    fn a_zero_response_queue_is_rejected_before_startup() {
        let mut configuration = NodeConfig::dev_default();
        configuration.response_queue_bytes = 0;
        assert!(
            matches!(
                configuration.validate(),
                Err(ConfigError::ZeroResponseQueue)
            ),
            "a zero response queue must fail before any writer can wait forever"
        );
    }

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
    fn a_zero_in_flight_cap_is_refused_at_startup() {
        let mut c = NodeConfig::dev_default();
        c.proxy.max_in_flight = 0;
        assert_eq!(
            c.validate(),
            Err(ConfigError::ZeroInFlight(0)),
            "a node that admits nothing is not a node"
        );
    }

    #[test]
    fn a_version_key_the_daemon_does_not_read_is_refused_at_load() {
        let err = NodeConfig::from_toml(
            r#"
node = "n"
[[cells]]
name = "c"
[[apps]]
name = "a"
tenant = "t"
limits.module_bytes_compressed = 20971520
limits.instance_memory_max = 134217728
limits.cpu_ms_per_http_dispatch = 30000
limits.subrequests_per_dispatch = 200
limits.pending_futures_per_dispatch = 64
cell = "c"
versions = [{ id = "v1", tier = "fast" }]
"#,
        )
        .expect_err("a removed key must not load silently");
        assert!(
            err.to_string().contains("tier"),
            "the refusal must name the key so the operator can delete it: {err}"
        );
    }

    #[test]
    fn a_cell_key_the_daemon_does_not_read_is_refused_at_load() {
        let err = NodeConfig::from_toml(
            r#"
node = "n"
[[cells]]
name = "c"
max_hot = 16
"#,
        )
        .expect_err("a removed key must not load silently");
        assert!(
            err.to_string().contains("max_hot"),
            "the refusal must name the key so the operator can delete it: {err}"
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
