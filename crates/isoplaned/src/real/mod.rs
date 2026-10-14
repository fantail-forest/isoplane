//! Local preparation and execution cells behind the HTTP dispatcher.

mod admin;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use isoplane_abi::dispatch::HttpRequest;
use isoplane_abi::host::{Dispatcher, HttpReply};
use isoplane_host::ModuleId;
use isoplane_host::{Cell, CellConfig, EngineActivator};
use parking_lot::RwLock;

pub use admin::RealNodeAdmin;

/// The read-only files every guest sees, at pre-initialisation and at
/// dispatch alike: today the trust bundle alone.
pub fn platform_files(
    trust: &crate::trust::TrustBundle,
) -> Result<isoplane_engine::PlatformFiles, isoplane_engine::PlatformFilesError> {
    isoplane_engine::PlatformFiles::new(vec![isoplane_engine::PlatformFile {
        path: crate::trust::GUEST_PATH.to_string(),
        bytes: trust.pem().into(),
        modified_ns: trust.modified_ns(),
    }])
}

/// The engine sizing from the node's config plus the platform files.
pub fn engine_config(
    cfg: &crate::config::NodeConfig,
    trust: &crate::trust::TrustBundle,
) -> Result<isoplane_engine::EngineConfig, isoplane_engine::PlatformFilesError> {
    Ok(isoplane_engine::EngineConfig {
        perf_jitdump: cfg.engine.perf_jitdump,
        max_instances: cfg.engine.max_instances,
        total_tables: cfg.engine.total_tables,
        max_memory_bytes: cfg.engine.max_memory_mib << 20,
        guard_size_bytes: cfg.engine.guard_mib << 20,
        max_core_instance_size_bytes: cfg.engine.max_core_instance_size_bytes,
        table_elements: cfg.engine.table_elements,
        max_tables_per_module: cfg.engine.max_tables_per_module,
        compiler_stack_bytes: cfg.engine.compiler_stack_bytes,
        max_wasm_stack_bytes: cfg.engine.max_wasm_stack_bytes,
        platform_files: platform_files(trust)?,
        ..isoplane_engine::EngineConfig::default()
    })
}

/// The node's cells on one engine, and the placement that puts an app on
/// one of them.
pub struct RealCells {
    runtime_settings: Arc<dyn crate::development::RuntimeSettingsSource>,
    engine: Arc<isoplane_engine::IsoplaneEngine>,
    /// Compiled template artifacts, mapped rather than read, so a restart
    /// does not recompile.
    artifacts: std::path::PathBuf,
    cells: Vec<Arc<Cell>>,
    /// Configured application-to-cell assignment.
    placement: RwLock<HashMap<String, usize>>,
    /// Exact app/version identities registered on this node.
    modules: RwLock<HashSet<ModuleId>>,
    /// app -> tenant.
    tenants: Arc<RwLock<HashMap<String, String>>>,
    /// The only way a module reaches a cell.
    deployer: Arc<crate::deploy::Deployer>,
}

/// What a real node keeps on disk.
pub struct RealPaths {
    /// Private local preparation output.
    pub registry: std::path::PathBuf,
    /// Compiled templates, one `.meta` and `.cwasm` pair per
    /// `(module hash, engine version, micro-architecture)`. Apart from the
    /// registry because these files are mapped and never rewritten.
    pub artifacts: std::path::PathBuf,
}

impl RealPaths {
    pub fn under(root: &Path) -> Self {
        RealPaths {
            registry: root.join("registry"),
            artifacts: root.join("artifacts"),
        }
    }
}

impl RealCells {
    pub fn cells(&self) -> &[Arc<Cell>] {
        &self.cells
    }

    /// Local preparation using this node's existing compiler-owned artifact cache.
    pub fn local_preparation(&self) -> anyhow::Result<isoplane_prepare::LocalPreparation> {
        // SAFETY: the node owns this cache and uses only the atomic, content-addressed
        // compiler writer, the same provenance required by its legacy mapped loader.
        let cache = unsafe {
            isoplane_engine::CompilationCache::from_trusted_directory(self.artifacts.clone())
        }?;
        Ok(isoplane_prepare::LocalPreparation::with_cache(Arc::new(
            cache,
        ))?)
    }

    /// Registers a prepared template without acquiring artifacts or invoking preparation.
    /// With the receipt of its published artifact, the template is restored from it again
    /// once a read of its mapped storage fails.
    pub async fn install_prepared(
        &self,
        app: &str,
        version: &str,
        tenant: &str,
        manifest: isoplane_abi::config::AppManifest,
        template: isoplane_engine::Template,
        receipt: Option<crate::deploy::PreparedReceipt>,
    ) -> anyhow::Result<ModuleId> {
        let cell = self.cell_for(app)?;
        let module = ModuleId::new(app, version);
        let activator = match receipt {
            Some(receipt) => {
                let deployer = self.deployer.clone();
                let engine = self.engine.clone();
                EngineActivator::reloading(
                    self.engine.clone(),
                    template,
                    module.clone(),
                    Box::new(move || deployer.restore_prepared(&engine, &receipt)),
                )
            }
            None => EngineActivator::new(self.engine.clone(), template, module.clone()),
        };
        let settings = self
            .runtime_settings
            .settings(crate::development::SettingsOwner {
                tenant,
                application: app,
            })?;
        cell.register_module_with_exceptions(
            tenant,
            Arc::new(activator),
            manifest,
            settings.allowed_destinations.into(),
        )?;
        cell.set_runtime_configuration(tenant, &module, settings.configuration)?;
        self.tenants
            .write()
            .insert(app.to_owned(), tenant.to_owned());
        self.modules.write().insert(module.clone());
        tracing::info!(app, version, cell = %cell.cfg.name, "prepared module registered");
        Ok(module)
    }

    /// The exact execution engine used to admit and activate this node's prepared templates.
    pub fn engine(&self) -> &isoplane_engine::IsoplaneEngine {
        &self.engine
    }
}

/// Dispatches to the application's configured local cell.
#[async_trait::async_trait]
impl Dispatcher for RealCells {
    async fn dispatch_http(
        &self,
        target: &isoplane_abi::host::RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<HttpReply> {
        self.cell_for(&target.app)?.dispatch_http(target, req).await
    }

    async fn dispatch_http_stream(
        &self,
        target: &isoplane_abi::host::RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<isoplane_abi::host::HttpExchange> {
        self.cell_for(&target.app)?
            .dispatch_http_stream(target, req)
            .await
    }
    async fn dispatch_http_upload(
        &self,
        target: &isoplane_abi::host::RouteTarget,
        upload: isoplane_abi::host::HttpUpload,
    ) -> anyhow::Result<isoplane_abi::host::HttpExchange> {
        self.cell_for(&target.app)?
            .dispatch_http_upload(target, upload)
            .await
    }
}

fn initialization_network(
    tenant: &str,
    manifest: &isoplane_abi::config::AppManifest,
    allowed_destinations: Vec<std::net::SocketAddr>,
    limits: isoplane_runtime::network::NetworkLimits,
    host: &isoplane_host::CellConfig,
) -> isoplane_prepare::InitializationNetwork {
    isoplane_prepare::InitializationNetwork {
        tenant: tenant.to_owned(),
        allowlist: manifest.bindings.fetch_allowlist.clone(),
        destinations: isoplane_prepare::InitializationDestinations::Public {
            exceptions: allowed_destinations,
        },
        limits,
        native_handles: host.guest_resources.native_handles,
        pending_futures: manifest.effective_limits().pending_futures_per_dispatch,
    }
}

#[cfg(test)]
mod engine_policy_tests {
    #[test]
    fn preparation_uses_the_application_owner_and_execution_network_budgets() {
        let mut config = crate::config::NodeConfig::dev_default();
        config.network.instance_pending_connections = 1;
        config.network.instance_open_connections = 2;
        config.host.guest_resources.native_handles = 7;
        let manifest: isoplane_abi::config::AppManifest = toml::from_str(
            "name='app'\n[bindings]\nfetch_allowlist=['db.example:5432']\n[resource_limits]\npending_futures_per_dispatch=2",
        ).expect("application policy");
        let endpoint = "127.0.0.1:5432".parse().expect("local database endpoint");
        let policy = super::initialization_network(
            "owner",
            &manifest,
            vec![endpoint],
            config.network,
            &config.host,
        );
        assert_eq!(
            policy.tenant, "owner",
            "initialization must retain the established owner"
        );
        assert_eq!(
            policy.allowlist, manifest.bindings.fetch_allowlist,
            "application egress restrictions must also apply before capture"
        );
        assert_eq!(
            policy.destinations,
            isoplane_prepare::InitializationDestinations::Public {
                exceptions: vec![endpoint]
            },
            "the explicit local database allowance must reach preparation"
        );
        assert_eq!(
            (
                policy.limits.instance_pending_connections,
                policy.limits.instance_open_connections
            ),
            (1, 2),
            "initialization must obey the host connection limits"
        );
        assert_eq!(
            policy.native_handles, 7,
            "initialization must obey the host native resource limit"
        );
        assert_eq!(
            policy.pending_futures, 2,
            "initialization must obey application resource overrides"
        );
    }

    #[test]
    fn operator_profiling_configuration_reaches_the_engine() {
        let mut config = crate::config::NodeConfig::dev_default();
        let trust = config.trust_bundle().expect("the bundled roots must load");
        assert!(
            !super::engine_config(&config, &trust)
                .expect("valid engine configuration")
                .perf_jitdump,
            "normal node operation must not write profiling artifacts"
        );
        config.engine = toml::from_str("perf_jitdump=true")
            .expect("operator profiling configuration must deserialize");
        assert!(
            super::engine_config(&config, &trust)
                .expect("valid engine configuration")
                .perf_jitdump,
            "an operator must be able to enable native guest symbols"
        );
    }

    #[test]
    fn configured_engine_resource_limits_reach_the_execution_engine() {
        let mut config = crate::config::NodeConfig::dev_default();
        config.engine = toml::from_str(
            "max_core_instance_size_bytes=8388608\ntable_elements=262144\nmax_tables_per_module=8\ncompiler_stack_bytes=16777216\nmax_wasm_stack_bytes=1048576\ntotal_tables=20",
        ).expect("operator engine limits must deserialize");
        let trust = config.trust_bundle().expect("the bundled roots must load");
        let engine = super::engine_config(&config, &trust).expect("platform files must be valid");
        assert_eq!(
            engine.total_tables,
            Some(20),
            "aggregate table slots must reach the engine"
        );
        assert_eq!(
            engine.max_wasm_stack_bytes,
            1 << 20,
            "guest stack limit must reach the engine"
        );
        assert_eq!(
            engine.max_core_instance_size_bytes,
            8 << 20,
            "node metadata limit must reach the engine"
        );
        assert_eq!(
            engine.table_elements, 262144,
            "node table size must reach the engine"
        );
        assert_eq!(
            engine.max_tables_per_module, 8,
            "node table count must reach the engine"
        );
        assert_eq!(
            engine.compiler_stack_bytes,
            16 << 20,
            "node compiler stack must reach the engine"
        );
    }
}
