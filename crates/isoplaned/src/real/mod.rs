//! The local data plane behind the proxy's `Dispatcher`.
//!
//! Connects configured application versions to the shared engine and execution pool.

mod admin;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use isoplane_abi::dispatch::HttpRequest;
use isoplane_abi::host::{Dispatcher, HttpReply};
use isoplane_host::cell::Services;
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
        max_memory_bytes: isoplane_abi::config::memory_ceiling_bytes(
            cfg.engine.memory_limit_mib << 20,
        ),
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

/// The node's cells on one engine, and the cell hosting each registered version.
pub struct RealCells {
    engine: Arc<isoplane_engine::IsoplaneEngine>,
    preparation: Option<crate::preparation::PreparationConfig>,
    /// The shared runtimes application packages are prepared on.
    runtimes: Option<crate::runtimes::Runtimes>,
    /// Compiled template artifacts, mapped rather than read, so a restart
    /// does not recompile.
    artifacts: std::path::PathBuf,
    cells: Vec<Arc<Cell>>,
    /// Applications admitted by the operator configuration.
    configured_applications: HashSet<String>,
    /// Exact deployments registered in this process.
    modules: RwLock<HashSet<ModuleId>>,
    registration: tokio::sync::Mutex<()>,
    /// app -> tenant.
    tenants: Arc<RwLock<HashMap<String, String>>>,
    /// Controlled local artifact preparation and restoration for this host.
    pub deployer: Arc<crate::deploy::Deployer>,
}

/// What a real node keeps on disk.
pub struct RealPaths {
    /// Local pressure snapshot reservations.
    pub snapshots: std::path::PathBuf,
    /// The private artifact store populated by this host's compiler.
    pub registry: std::path::PathBuf,
    /// Compiled templates, one `.meta` and `.cwasm` pair per
    /// `(module hash, engine version, micro-architecture)`. Apart from the
    /// registry because these files are mapped and never rewritten.
    pub artifacts: std::path::PathBuf,
}

impl RealPaths {
    pub fn under(root: &Path) -> Self {
        RealPaths {
            snapshots: root.join("snapshots"),
            registry: root.join("registry"),
            artifacts: root.join("artifacts"),
        }
    }
}

impl RealCells {
    /// Returns the deployment currently registered for this owner's application.
    pub fn registered_deployment(&self, app: &str, owner: &str) -> Option<String> {
        if self.tenants.read().get(app).map(String::as_str) != Some(owner) {
            return None;
        }
        self.modules
            .read()
            .iter()
            .find(|module| module.app == app)
            .map(|module| module.version.clone())
    }

    /// Builds one shared execution pool with per-application sandbox and resource bounds.
    pub fn build(cfg: &crate::config::NodeConfig, paths: &RealPaths) -> anyhow::Result<Arc<Self>> {
        Self::build_with_destinations(
            cfg,
            paths,
            Arc::new(isoplane_host::egress::PublicDestinations),
            &cfg.trust_bundle()?,
        )
    }

    /// Builds cells sharing the address policy used by their outbound
    /// transports, serving `trust` to every guest as its root store.
    pub fn build_with_destinations(
        cfg: &crate::config::NodeConfig,
        paths: &RealPaths,
        destinations: Arc<dyn isoplane_host::egress::DestinationPolicy>,
        trust: &crate::trust::TrustBundle,
    ) -> anyhow::Result<Arc<Self>> {
        let network = cfg.network;
        let egress = Arc::new(
            isoplane_host::HostEgressPolicy::system(cfg.connection_limits()?)?
                .with_destinations(destinations),
        );
        let engine = Arc::new(isoplane_engine::IsoplaneEngine::new(engine_config(
            cfg, trust,
        )?)?);

        let tenants: Arc<RwLock<HashMap<String, String>>> = Arc::new(RwLock::new(HashMap::new()));
        let fetcher = Arc::new(isoplane_host::PinnedHttpFetcher::new(
            network.inline_response_bytes,
        ));

        let mut cells = Vec::with_capacity(1);
        {
            let name = &cfg.host.name;
            let services = Arc::new(Services {
                egress: egress.clone(),
                fetcher: fetcher.clone(),
            });
            cells.push(Cell::with_snapshot_storage(
                CellConfig {
                    name: name.clone(),

                    response_queue_bytes: cfg.response_queue_bytes,
                    ..cfg.host.clone()
                },
                services,
                Arc::new(isoplane_hibernate::snapshot::LocalSnapshotStorage::new(
                    &paths.snapshots,
                )?),
            )?);
        }

        if let Some(preparation) = &cfg.preparation {
            preparation.validate()?;
        }
        let runtimes = match cfg.preparation.as_ref().and_then(|p| p.runtimes.as_deref()) {
            Some(directory) => {
                let runtimes = crate::runtimes::Runtimes::load(directory)?;
                tracing::info!(
                    directory = %directory.display(),
                    sdk_versions = ?runtimes.sdk_versions().collect::<Vec<_>>(),
                    "installed shared runtimes checked"
                );
                Some(runtimes)
            }
            None => None,
        };
        let configured_applications = cfg.apps.iter().map(|a| a.manifest.name.clone()).collect();
        for a in &cfg.apps {
            tenants
                .write()
                .insert(a.manifest.name.clone(), cfg.owner.clone());
        }

        let deployer = Arc::new(crate::deploy::Deployer::open(&paths.registry)?);
        tracing::info!(
            registry = %paths.registry.display(),
            "deploy pipeline ready (validate -> pre-init -> compile -> publish -> register)"
        );

        isoplane_engine::CompilationCache::create_directory(&paths.artifacts)?;
        let me = Arc::new(RealCells {
            engine,
            preparation: cfg.preparation.clone(),
            runtimes,
            artifacts: paths.artifacts.clone(),
            cells,
            configured_applications,
            modules: RwLock::new(HashSet::new()),
            registration: tokio::sync::Mutex::new(()),
            tenants,
            deployer,
        });
        Ok(me)
    }

    pub fn cells(&self) -> &[Arc<Cell>] {
        &self.cells
    }

    fn ensure_configured(&self, app: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.configured_applications.contains(app),
            "application `{app}` is not configured on this host"
        );
        Ok(())
    }

    fn cell_for(&self, app: &str, version: &str) -> anyhow::Result<Arc<Cell>> {
        let module = ModuleId::new(app, version);
        anyhow::ensure!(
            self.modules.read().contains(&module),
            "deployment `{module}` is not registered on this host"
        );
        self.cells
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("host execution pool is absent"))
    }

    /// Restore a locally selected application without compilation or initialization.
    pub async fn restore_application(
        &self,
        tenant: &str,
        installed: crate::installed::InstalledApplication,
    ) -> anyhow::Result<ModuleId> {
        let receipt = &installed.receipt;
        self.preparation.as_ref().ok_or_else(|| {
            anyhow::anyhow!("restoration requires installed runtime configuration")
        })?;
        match &receipt.runtime_profile {
            isoplane_prepare::RuntimeProfile::SharedRuntime { sdk_version } => anyhow::ensure!(
                self.runtimes
                    .as_ref()
                    .is_some_and(|runtimes| runtimes.supports(sdk_version)),
                "installed deployment {} was prepared on SDK {sdk_version}'s runtime, which this node does not install",
                receipt.version
            ),
            isoplane_prepare::RuntimeProfile::RawWasm => anyhow::bail!("installed deployments must be prepared from an application package on an installed shared runtime"),
        }
        self.install_application(tenant, installed).await
    }

    /// Local preparation using this node's existing compiler-owned artifact cache.
    pub fn local_preparation(&self) -> anyhow::Result<isoplane_prepare::LocalPreparation> {
        // SAFETY: the node owns this cache and uses only the atomic, content-addressed
        // compiler writer required by the mapped loader.
        let cache = unsafe {
            isoplane_engine::CompilationCache::from_trusted_directory(self.artifacts.clone())
        }?;
        Ok(isoplane_prepare::LocalPreparation::with_cache(Arc::new(
            cache,
        ))?)
    }

    /// Loads a newly prepared publication, retaining its backing lease in the activator.
    pub async fn install_application(
        &self,
        owner: &str,
        installed: crate::installed::InstalledApplication,
    ) -> anyhow::Result<ModuleId> {
        let (receipt, lease) = installed.into_parts();
        self.ensure_configured(&receipt.manifest.name)?;
        let template = self.deployer.restore_prepared(&self.engine, &receipt)?;
        self.install_prepared_on_cell(
            ModuleId::new(&receipt.manifest.name, &receipt.version),
            owner,
            receipt.manifest.clone(),
            template,
            Some(receipt.clone()),
            Some(lease),
        )
        .await
    }

    async fn install_prepared_on_cell(
        &self,
        module: ModuleId,
        tenant: &str,
        manifest: isoplane_abi::config::AppManifest,
        template: isoplane_engine::Template,
        receipt: Option<crate::deploy::PreparedReceipt>,
        lease: Option<Arc<isoplane_prepare::PreparationOwner>>,
    ) -> anyhow::Result<ModuleId> {
        let _registration = self.registration.lock().await;
        anyhow::ensure!(
            !self
                .modules
                .read()
                .iter()
                .any(|registered| registered.app == module.app),
            "application `{}` is already registered; deployment `{}` requires a host restart",
            module.app,
            module.version
        );
        anyhow::ensure!(
            !tenant.trim().is_empty() && manifest.name == module.app,
            "module registration requires the established application and owner"
        );
        if let Some(owner) = self.tenants.read().get(&module.app) {
            anyhow::ensure!(
                owner == tenant,
                "module registration cannot change application ownership"
            );
        }
        let cell = self
            .cells
            .first()
            .ok_or_else(|| anyhow::anyhow!("host execution pool is absent"))?;
        let settings = match receipt.as_ref() {
            Some(receipt) => {
                anyhow::ensure!(
                    receipt.manifest.name == module.app && receipt.version == module.version,
                    "prepared settings receipt does not match the registered application version"
                );
                self.deployer.runtime_settings(tenant, receipt).await?
            }
            None => crate::settings::RuntimeSettings::default(),
        };
        let activator = match receipt {
            Some(receipt) => {
                let deployer = self.deployer.clone();
                let engine = self.engine.clone();
                EngineActivator::reloading(
                    self.engine.clone(),
                    template,
                    module.clone(),
                    Box::new(move || {
                        let _held = &lease;
                        deployer.restore_prepared(&engine, &receipt)
                    }),
                )
            }
            None => EngineActivator::new(self.engine.clone(), template, module.clone()),
        };
        cell.register_module_with_exceptions(
            tenant,
            Arc::new(activator),
            manifest,
            settings.allowed_destinations.into(),
        )?;
        cell.set_runtime_configuration(tenant, &module, settings.configuration)?;
        self.tenants
            .write()
            .insert(module.app.clone(), tenant.to_owned());
        self.modules.write().insert(module.clone());
        tracing::info!(app = %module.app, version = %module.version, cell = %cell.cfg.name, "prepared module registered");
        Ok(module)
    }

    /// The exact execution engine used to admit and activate this node's prepared templates.
    pub fn engine(&self) -> &isoplane_engine::IsoplaneEngine {
        &self.engine
    }
}

/// Exact registered deployment dispatch through the shared execution pool.
#[async_trait::async_trait]
impl Dispatcher for RealCells {
    async fn dispatch_http(
        &self,
        target: &isoplane_abi::host::RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<HttpReply> {
        self.cell_for(&target.app, &target.version)?
            .dispatch_http(target, req)
            .await
    }

    async fn dispatch_http_stream(
        &self,
        target: &isoplane_abi::host::RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<isoplane_abi::host::HttpExchange> {
        self.cell_for(&target.app, &target.version)?
            .dispatch_http_stream(target, req)
            .await
    }
    async fn dispatch_http_upload(
        &self,
        target: &isoplane_abi::host::RouteTarget,
        upload: isoplane_abi::host::HttpUpload,
    ) -> anyhow::Result<isoplane_abi::host::HttpExchange> {
        self.cell_for(&target.app, &target.version)?
            .dispatch_http_upload(target, upload)
            .await
    }
}

pub(crate) fn initialization_network(
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
        open_connections: manifest.effective_limits().instance_open_connections,
        pending_futures: manifest.effective_limits().pending_futures_per_dispatch,
        buffered_bytes: host.buffered_future_bytes,
    }
}

#[cfg(test)]
mod tests;
