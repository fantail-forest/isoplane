//! Node configuration and startup validation.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use isoplane_abi::config::{AppManifest, DEFAULT_ADMIN_PORT, DEFAULT_PROXY_PORT};
use serde::{Deserialize, Serialize};

/// One owner's local applications, listeners, and execution policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    /// Per-application protected local file references and exact endpoint exceptions.
    #[serde(default)]
    pub application_settings: Vec<crate::settings::ApplicationSettings>,
    #[serde(default = "default_node_name")]
    pub node: String,
    /// Explicit local owner namespace for every application.
    pub owner: String,
    #[serde(default)]
    pub proxy: ProxyListen,
    #[serde(default)]
    pub admin: AdminListen,
    #[serde(default)]
    pub apps: Vec<AppConfig>,
    /// Protected local installation, artifacts and pressure-hibernation storage.
    #[serde(default = "default_data_dir")]
    pub data_dir: std::path::PathBuf,
    #[serde(default)]
    pub engine: EngineLimits,
    /// Host execution, queue, lifecycle and per-instance resource budgets.
    #[serde(default)]
    pub host: isoplane_host::CellConfig,
    /// Per-instance outbound connection limits and response byte budgets.
    #[serde(default)]
    pub network: isoplane_runtime::network::NetworkLimits,

    /// High-water mark for each real cell's queued HTTP response bytes.
    #[serde(default = "default_response_queue_bytes")]
    pub response_queue_bytes: usize,
    /// Full OTLP HTTP/protobuf endpoint for structured dispatch events. Unset disables export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_endpoint: Option<crate::logs::OtlpEndpoint>,
    /// Full OTLP HTTP/protobuf endpoint for lifecycle spans. Unset disables trace export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_traces_endpoint: Option<crate::logs::OtlpEndpoint>,
    /// Per-process limits; incomplete and over-limit traces are discarded in full.
    #[serde(default)]
    pub trace_limits: crate::logs::TraceLimits,
    /// The public TLS root bundle guests read at
    /// `/etc/ssl/certs/ca-certificates.crt`.
    #[serde(default)]
    pub trust: crate::trust::TrustConfig,
    /// Installed runtime location and bounded local initialization policy.
    /// Required, including `runtimes`, when applications are configured.
    #[serde(default)]
    pub preparation: Option<crate::preparation::PreparationConfig>,
}

fn default_response_queue_bytes() -> usize {
    isoplane_abi::host::DEFAULT_RESPONSE_QUEUE_BYTES
}

/// The engine sizing a real node's cells share. The pooling allocator
/// reserves `max_instances × (max_memory + guard)` of address space up front,
/// so several nodes on one machine have to be able to say "smaller".
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineLimits {
    /// Emit Linux perf jitdump records in the process working directory.
    #[serde(default)]
    pub perf_jitdump: bool,
    /// Ceiling on concurrently resident instances per engine.
    #[serde(default = "default_max_instances")]
    pub max_instances: u32,
    /// Aggregate pooled table slots; omission follows the instance count.
    #[serde(default)]
    pub total_tables: Option<u32>,
    /// The largest application memory limit this node serves. An instance may
    /// grow a quarter past its limit before the host stops it, so each engine
    /// slot is sized for that ceiling.
    #[serde(default = "default_memory_limit_mib")]
    pub memory_limit_mib: u64,
    /// The guard region after each memory, which lets Cranelift elide
    /// bounds checks. Address space, not memory.
    #[serde(default = "default_guard_mib")]
    pub guard_mib: u64,
    /// Maximum metadata bytes retained by one pooled core instance.
    #[serde(default = "default_core_instance_bytes")]
    pub max_core_instance_size_bytes: usize,
    /// Maximum elements in each guest function table.
    #[serde(default = "default_table_elements")]
    pub table_elements: usize,
    /// Maximum function tables declared by one guest module.
    #[serde(default = "default_tables_per_module")]
    pub max_tables_per_module: u32,
    /// Native stack bytes reserved per compiler worker.
    #[serde(default = "default_compiler_stack_bytes")]
    pub compiler_stack_bytes: usize,
    /// Maximum native stack bytes consumed by one slice.
    #[serde(default = "default_wasm_stack_bytes")]
    pub max_wasm_stack_bytes: usize,
}

fn default_wasm_stack_bytes() -> usize {
    isoplane_abi::config::DEFAULT_WASM_STACK_BYTES
}
fn default_compiler_stack_bytes() -> usize {
    isoplane_abi::config::DEFAULT_COMPILER_STACK_BYTES
}
fn default_core_instance_bytes() -> usize {
    isoplane_abi::config::DEFAULT_CORE_INSTANCE_SIZE_BYTES
}
fn default_table_elements() -> usize {
    isoplane_abi::config::DEFAULT_TABLE_ELEMENTS
}
fn default_tables_per_module() -> u32 {
    isoplane_abi::config::DEFAULT_TABLES_PER_MODULE
}
fn default_max_instances() -> u32 {
    isoplane_abi::config::DEFAULT_MAX_INSTANCES
}
fn default_memory_limit_mib() -> u64 {
    isoplane_abi::config::GuestMemoryBytes::default().get() >> 20
}
fn default_guard_mib() -> u64 {
    isoplane_abi::config::DEFAULT_MEMORY_GUARD_BYTES >> 20
}

impl EngineLimits {
    fn validate(&self) -> Result<(), ConfigError> {
        if self.max_instances == 0
            || self.max_wasm_stack_bytes == 0
            || self.total_tables == Some(0)
            || self.compiler_stack_bytes == 0
            || self.table_elements == 0
            || self.max_tables_per_module == 0
            || self.max_core_instance_size_bytes == 0
        {
            return Err(ConfigError::InvalidEngine(
                "instance, compiler stack and pooling capacities must be positive".into(),
            ));
        }
        let memory = self
            .memory_limit_mib
            .checked_mul(1 << 20)
            .ok_or_else(|| ConfigError::InvalidEngine("memory_limit_mib overflows bytes".into()))?;
        isoplane_abi::config::GuestMemoryBytes::new(memory)
            .map_err(|error| ConfigError::InvalidEngine(error.to_string()))?;
        self.guard_mib
            .checked_mul(1 << 20)
            .ok_or_else(|| ConfigError::InvalidEngine("guard_mib overflows bytes".into()))?;
        Ok(())
    }
}

impl Default for EngineLimits {
    fn default() -> Self {
        EngineLimits {
            perf_jitdump: false,
            max_instances: default_max_instances(),
            total_tables: None,
            memory_limit_mib: default_memory_limit_mib(),
            guard_mib: default_guard_mib(),
            max_core_instance_size_bytes: default_core_instance_bytes(),
            table_elements: default_table_elements(),
            max_tables_per_module: default_tables_per_module(),
            compiler_stack_bytes: default_compiler_stack_bytes(),
            max_wasm_stack_bytes: default_wasm_stack_bytes(),
        }
    }
}

fn default_data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("./isoplaned-data")
}

fn default_node_name() -> String {
    "isoplaned-0".into()
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

/// Application policy and its exact expected Host names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Policy captured with the prepared application.
    pub manifest: AppManifest,
    /// Hostnames without schemes, paths or ports.
    pub hosts: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("invalid application settings: {0}")]
    Settings(String),
    #[error("invalid engine configuration: {0}")]
    InvalidEngine(String),
    #[error("invalid preparation configuration: {0}")]
    Preparation(String),
    #[error("owner must be a nonempty explicit local identity")]
    Owner,
    #[error("duplicate app name `{0}`")]
    DuplicateApp(String),
    #[error("invalid application routing: {0}")]
    Routing(String),
    #[error("proxy.max_in_flight must be positive (got {0})")]
    ZeroInFlight(usize),
    #[error("response_queue_bytes must be positive")]
    ZeroResponseQueue,
    #[error("invalid host settings: {0}")]
    InvalidHost(String),
    #[error("invalid [network] connection limits: {0}")]
    InvalidNetwork(isoplane_host::egress::ConnectionLimitsError),
    #[error(transparent)]
    Trust(#[from] crate::trust::TrustError),
}

impl NodeConfig {
    /// Parses configuration without opening settings or installation state.
    pub fn from_toml(s: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(s)?)
    }

    /// Loads local paths relative to the configuration file's directory.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let path = path.canonicalize()?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("configuration has no parent directory"))?;
        let mut config = Self::from_toml(&std::fs::read_to_string(&path)?)?;
        if config.data_dir.is_relative() {
            config.data_dir = parent.join(&config.data_dir);
        }
        {
            for entry in &mut config.application_settings {
                if let Some(file) = &mut entry.configuration_file {
                    if file.is_relative() {
                        *file = parent.join(&*file);
                    }
                }
            }
            if let Some(preparation) = &mut config.preparation {
                if let Some(directory) = &mut preparation.runtimes {
                    if directory.is_relative() {
                        *directory = parent.join(&*directory);
                    }
                }
            }
        }
        if let Some(bundle) = &mut config.trust.bundle {
            if bundle.is_relative() {
                *bundle = parent.join(&*bundle);
            }
        }
        Ok(config)
    }

    /// Creates an empty host with an explicitly established owner.
    pub fn new(owner: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            node: default_node_name(),
            application_settings: Vec::new(),
            proxy: Default::default(),
            admin: Default::default(),
            apps: Vec::new(),
            data_dir: default_data_dir(),
            engine: Default::default(),
            host: Default::default(),
            network: Default::default(),
            response_queue_bytes: default_response_queue_bytes(),
            otlp_logs_endpoint: None,
            otlp_traces_endpoint: None,
            trace_limits: Default::default(),
            trust: Default::default(),
            preparation: None,
        }
    }

    /// Refuses invalid static settings before opening services or binding listeners.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.owner.trim().is_empty() {
            return Err(ConfigError::Owner);
        }
        self.engine.validate()?;
        {
            crate::settings::validate(&self.application_settings)
                .map_err(|e| ConfigError::Settings(e.to_string()))?;
            for settings in &self.application_settings {
                if !self
                    .apps
                    .iter()
                    .any(|app| app.manifest.name == settings.application)
                {
                    return Err(ConfigError::Settings(format!(
                        "settings for `{}` must belong to a configured application and host owner",
                        settings.application
                    )));
                }
            }
            self.host
                .validate()
                .map_err(|e| ConfigError::InvalidHost(e.to_string()))?;
            self.connection_limits()?;
            if let Some(preparation) = &self.preparation {
                preparation
                    .validate()
                    .map_err(|e| ConfigError::Preparation(e.to_string()))?;
            }
        }
        if self.response_queue_bytes == 0 {
            return Err(ConfigError::ZeroResponseQueue);
        }
        if self.proxy.max_in_flight == 0 {
            return Err(ConfigError::ZeroInFlight(0));
        }
        let mut hosts = std::collections::HashMap::new();
        let mut names = BTreeSet::new();
        for app in &self.apps {
            if !names.insert(&app.manifest.name) {
                return Err(ConfigError::DuplicateApp(app.manifest.name.clone()));
            }
            if app.manifest.name.trim().is_empty() || app.hosts.is_empty() {
                return Err(ConfigError::Routing(
                    "each application needs a name and at least one host".into(),
                ));
            }
            for host in &app.hosts {
                let normalized = isoplane_proxy::routes::configured_host(host)
                    .map_err(|error| ConfigError::Routing(error.to_string()))?;
                if let Some(previous) = hosts.insert(normalized, &app.manifest.name) {
                    return Err(ConfigError::Routing(format!(
                        "host `{host}` is repeated by `{previous}` and `{}`",
                        app.manifest.name
                    )));
                }
            }
            if app.manifest.memory_max_bytes() > self.engine.memory_limit_mib << 20 {
                return Err(ConfigError::InvalidEngine(format!(
                    "application `{}` exceeds engine memory_limit_mib",
                    app.manifest.name
                )));
            }
        }
        if !self.apps.is_empty()
            && self
                .preparation
                .as_ref()
                .and_then(|preparation| preparation.runtimes.as_deref())
                .is_none_or(|directory| directory.as_os_str().is_empty())
        {
            return Err(ConfigError::Preparation(
                "configured applications require preparation.runtimes to name the installed SDK runtime directory".into(),
            ));
        }
        self.trust_bundle().map(drop)
    }

    /// Validated outbound connection budgets.
    pub fn connection_limits(
        &self,
    ) -> Result<isoplane_host::egress::ConnectionLimits, ConfigError> {
        isoplane_host::egress::ConnectionLimits::new(isoplane_host::egress::ConnectionCeilings {
            in_progress: self.network.instance_pending_connections,
            open: self.network.instance_open_connections,
        })
        .map_err(ConfigError::InvalidNetwork)
    }

    /// Verified guest TLS root bundle.
    pub fn trust_bundle(&self) -> Result<crate::trust::TrustBundle, ConfigError> {
        Ok(crate::trust::TrustBundle::load(&self.trust)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_must_be_explicit_and_nonempty() {
        assert!(NodeConfig::from_toml("").is_err());
        NodeConfig::new("operator").validate().unwrap();
        assert_eq!(NodeConfig::new(" ").validate(), Err(ConfigError::Owner));
    }
    #[test]
    fn duplicate_normalized_hosts_fail_before_service_construction() {
        let config = NodeConfig::from_toml(
            r#"
owner = "operator"
[[apps]]
hosts = ["api.example"]
manifest = { name = "a" }
[[apps]]
hosts = ["API.example"]
manifest = { name = "b" }
"#,
        )
        .unwrap();
        assert!(matches!(config.validate(), Err(ConfigError::Routing(_))));
    }
    #[test]
    fn printed_configuration_preserves_operational_budgets() {
        let mut config = NodeConfig::new("operator");
        config.host.queue_depth_per_worker = 3;
        config.network.instance_open_connections = 9;
        config.host.buffered_future_bytes = 23;
        let restored = NodeConfig::from_toml(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(restored.host.queue_depth_per_worker, 3);
        assert_eq!(restored.network.instance_open_connections, 9);
        assert_eq!(restored.host.buffered_future_bytes, 23);
    }
}
