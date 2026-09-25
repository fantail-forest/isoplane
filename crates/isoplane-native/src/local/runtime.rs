//! One project's guest execution and local bindings, with no node control plane.

mod baseline;
mod compressed_module;
mod preparation_cache;

pub use baseline::Baselines;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use isoplane_abi::config::AppManifest;
use isoplane_abi::host::{DiId, Dispatcher, DurableStore, RouteTarget};
use isoplane_durable::activation::{ActivationConfig, ActivationRegistry, DiGuest};
use isoplane_host::{Cell, CellConfig, EngineActivator, ModuleId};
use isoplane_runtime::durable::{DurableBridge, DurableCalls, UnusedGuest};
use isoplane_runtime::sockets::{GatedSocketSink, ProxySocketOps};
use parking_lot::RwLock;

use super::Inputs as Project;

enum PreparedActivator {
    Ordinary(Arc<EngineActivator>),
    Debug(Arc<isoplane_host::debug::DebugActivator>),
}

/// Validated guest code ready to register with local services.
pub struct Prepared {
    activator: PreparedActivator,
    manifest: AppManifest,
    tenant: String,
    version: String,
    allow_endpoints: Vec<std::net::SocketAddr>,
    compiled: bool,
}

impl Prepared {
    /// Validates and pre-initializes an ordinary local application. An
    /// application package is prepared over the runtime baseline `baselines`
    /// holds, where the platform maps layers.
    pub fn build(
        project: &Project,
        wasm: &[u8],
        configuration: &isoplane_host::bindings::RuntimeConfiguration,
        baselines: &Baselines,
    ) -> Result<Self, super::Error> {
        Self::prepare(project, wasm, configuration, baselines).map_err(super::Error::Preparation)
    }

    fn prepare(
        project: &Project,
        wasm: &[u8],
        configuration: &isoplane_host::bindings::RuntimeConfiguration,
        baselines: &Baselines,
    ) -> anyhow::Result<Self> {
        let manifest = project.manifest.clone();
        let mut settings = Vec::new();
        configuration.write_json(&mut settings)?;
        let mut identity = blake3::Hasher::new();
        identity.update(blake3::hash(wasm).as_bytes());
        identity.update(&settings);
        preparation_cache::hash_files(&mut identity, &project.application_files);
        let version = identity.finalize().to_hex().to_string();
        let admit = || validate(project, wasm, false).map(drop);
        if let Some(prepared) = baselines.prepare(project, wasm, &settings, &version, admit) {
            return prepared;
        }
        let report = validate(project, wasm, false)?;
        let template = if report.has_initialize() {
            preparation_cache::template(project, wasm, &settings, || {
                preinitialize(project, wasm, &settings)
            })?
        } else {
            wasm.to_vec()
        };
        let engine = Arc::new(isoplane_engine::IsoplaneEngine::new(engine_configuration(
            project,
        ))?);
        let (template, compiled) = compilation_cache(project, blake3::hash(&template).as_bytes())?
            .load(&engine, &template)?;
        let template = template.with_application_files(project.application_files.clone())?;
        tracing::info!(compiled, "local native compilation ready");
        let module = ModuleId::new(&manifest.name, &version);
        let activator = Arc::new(EngineActivator::new(engine, template, module));
        Ok(Self {
            activator: PreparedActivator::Ordinary(activator),
            manifest,
            tenant: project.tenant(),
            version,
            allow_endpoints: project.allow_endpoints.clone(),
            compiled,
        })
    }
    /// Whether preparing this application required native compilation.
    pub fn compiled(&self) -> bool {
        self.compiled
    }
    /// Validates and compiles a marked debug module without pre-initialization.
    pub fn build_debug(project: &Project, wasm: &[u8]) -> Result<Self, super::Error> {
        Self::prepare_debug(project, wasm).map_err(super::Error::Preparation)
    }

    fn prepare_debug(project: &Project, wasm: &[u8]) -> anyhow::Result<Self> {
        validate(project, wasm, true)?;
        let manifest = project.manifest.clone();
        let mut identity = blake3::Hasher::new();
        identity.update(blake3::hash(wasm).as_bytes());
        preparation_cache::hash_files(&mut identity, &project.application_files);
        let version = identity.finalize().to_hex().to_string();
        let engine = Arc::new(isoplane_engine::IsoplaneEngine::new(
            isoplane_engine::EngineConfig {
                pooling: false,
                max_memory_bytes: manifest.memory_max_bytes(),
                platform_files: project.served_files()?,
                ..Default::default()
            },
        )?);
        let module = ModuleId::new(&manifest.name, &version);
        let activator = Arc::new(isoplane_host::debug::DebugActivator::load(
            engine, module, wasm,
        )?);
        Ok(Self {
            activator: PreparedActivator::Debug(activator),
            manifest,
            tenant: project.tenant(),
            version,
            allow_endpoints: project.allow_endpoints.clone(),
            compiled: true,
        })
    }
}

fn engine_configuration(project: &Project) -> isoplane_engine::EngineConfig {
    isoplane_engine::EngineConfig {
        pooling: false,
        max_memory_bytes: project.manifest.memory_max_bytes(),
        platform_files: project.platform_files.clone(),
        ..Default::default()
    }
}

struct LocalCompilation {
    cache: isoplane_engine::CompilationCache,
    lease: Arc<isoplane_cache::Lease>,
}

impl LocalCompilation {
    fn load(
        &self,
        engine: &isoplane_engine::IsoplaneEngine,
        wasm: &[u8],
    ) -> anyhow::Result<(isoplane_engine::Template, bool)> {
        let (mut template, compiled) = self.cache.load(engine, wasm)?;
        template.retain_storage(self.lease.clone());
        Ok((template, compiled))
    }
}

fn compilation_cache(
    project: &Project,
    artifact_identity: &[u8],
) -> anyhow::Result<LocalCompilation> {
    let mut identity = blake3::Hasher::new();
    identity.update_reader(std::fs::File::open(&project.runtime_library)?)?;
    identity.update(artifact_identity);
    let storage = isoplane_cache::Cache::new(
        project.compilation_directory.clone(),
        isoplane_cache::PROJECT_CACHE_BYTES,
    )?;
    let lease = Arc::new(storage.lease(isoplane_cache::CacheKey(*identity.finalize().as_bytes()))?);
    // SAFETY: only the compiler writes into this private, leased project directory.
    let cache = unsafe {
        isoplane_engine::CompilationCache::from_trusted_directory(lease.directory().into())
    }?;
    Ok(LocalCompilation { cache, lease })
}

fn preinitialize(project: &Project, wasm: &[u8], configuration: &[u8]) -> anyhow::Result<Vec<u8>> {
    let files = project.served_files()?;
    Ok(isoplane_prepare::PreInitializer::new()
        .with_memory_limit(isoplane_abi::config::GuestMemoryBytes::new(project.manifest.memory_max_bytes())?)
        .with_engine_config(isoplane_engine::EngineConfig { pooling: false, max_memory_bytes: project.manifest.memory_max_bytes(), platform_files: files.clone(), ..Default::default() })
        .with_network(initialization_network(project))
        .with_pal(project.init_sandbox)
        .with_runtime_configuration(configuration.to_vec())
        .with_platform_files(files)
        .run(wasm, &isoplane_prepare::PreInitRequest {
            module_hash: isoplane_engine::ModuleHash::of(wasm),
            init_blob: serde_json::to_vec(&project.manifest.init_visible_vars)?,
        })
        .map_err(|e| anyhow::anyhow!("pre-initializing {} with {}: {e}; managed guests may require dev.init_sandbox = 'runtime-bringup' in isoplane.toml", project.manifest.name, project.init_sandbox.as_str()))?
        .template_wasm)
}

fn initialization_network(project: &Project) -> isoplane_prepare::InitializationNetwork {
    let host = CellConfig::default();
    isoplane_prepare::InitializationNetwork {
        tenant: project.tenant(),
        allowlist: project.manifest.bindings.fetch_allowlist.clone(),
        allowed_destinations: project.allow_endpoints.clone(),
        limits: isoplane_runtime::network::NetworkLimits::default(),
        native_handles: host.guest_resources.native_handles,
        pending_futures: project
            .manifest
            .effective_limits()
            .pending_futures_per_dispatch,
    }
}

fn validate(
    project: &Project,
    wasm: &[u8],
    local_debug: bool,
) -> anyhow::Result<isoplane_runtime::ValidationReport> {
    let manifest = &project.manifest;
    let admission = if local_debug {
        isoplane_runtime::validate_local_debug_with_limits(
            wasm,
            manifest.limits,
            manifest.guest_memory_bytes,
            &manifest.validation_limits,
        )
    } else {
        isoplane_runtime::validate_with_limits(
            wasm,
            manifest.limits,
            manifest.guest_memory_bytes,
            &manifest.validation_limits,
        )
    };
    let report = admission.map_err(|violations| {
        anyhow::anyhow!(
            "module rejected:\n{}",
            violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;
    let compressed_bytes = compressed_module::size(&project.state_directory, wasm)?;
    let limit = manifest.effective_limits().module_bytes_compressed;
    anyhow::ensure!(compressed_bytes <= limit, "module is {compressed_bytes} compressed bytes, exceeding the configured {limit} byte limit");
    Ok(report)
}

/// A local application using the shared cell, proxy and persistent host services.
pub struct LocalApp {
    /// Owns guest dispatch and resource cleanup.
    pub cell: Arc<Cell>,
    /// Routes local HTTP through the shared request pipeline.
    pub proxy: Arc<isoplane_proxy::Proxy>,
    target: RouteTarget,
    _scratch: tempfile::TempDir,
    debug: Option<Arc<isoplane_host::debug::DebugActivator>>,
}

impl LocalApp {
    /// Stops debugger waits, sockets and guest workers before releasing session state.
    pub fn shutdown(&self) {
        if let Some(debug) = &self.debug {
            debug.stop();
        }
        self.proxy.shutdown_sockets();
        self.cell.shutdown();
    }

    /// Registers ordinary prepared code with local state.
    pub async fn start(
        prepared: Prepared,
        data: &Path,
        configuration: isoplane_host::bindings::RuntimeConfiguration,
    ) -> Result<Arc<Self>, super::Error> {
        Self::start_with_peer(prepared, data, None, configuration)
            .await
            .map_err(super::Error::Startup)
    }

    /// Registers raw debug code with the exact session peer allowed through egress.
    pub async fn start_debug(
        prepared: Prepared,
        data: &Path,
        peer: super::debug::DebugPeer,
    ) -> Result<Arc<Self>, super::Error> {
        Self::start_with_peer(
            prepared,
            data,
            Some(peer),
            isoplane_host::bindings::RuntimeConfiguration::default(),
        )
        .await
        .map_err(super::Error::Startup)
    }

    async fn start_with_peer(
        prepared: Prepared,
        data: &Path,
        peer: Option<super::debug::DebugPeer>,
        configuration: isoplane_host::bindings::RuntimeConfiguration,
    ) -> anyhow::Result<Arc<Self>> {
        let network = isoplane_runtime::network::NetworkLimits::default();
        let egress =
            isoplane_host::HostEgressPolicy::system(network.connections, network.egress_bytes)?;
        let egress =
            egress.with_destinations(Arc::new(super::destinations::DevelopmentDestinations::new(
                prepared.allow_endpoints.clone(),
                peer,
            )));
        let egress = Arc::new(egress);
        std::fs::create_dir_all(data)?;
        let scratch = tempfile::tempdir_in(data)?;
        let metrics = Arc::new(isoplane_proxy::ProxyMetrics::new());
        let cfg = isoplane_proxy::ProxyConfig::default();
        let hub = Arc::new(isoplane_proxy::SocketHub::new(metrics.clone(), cfg.sockets));
        let sockets = ProxySocketOps::new(hub.clone());
        let store = Arc::new(isoplane_store::SqliteStore::open(data.join("store.db"))?);
        let di_store: Arc<dyn DurableStore> = Arc::new(isoplane_store::SqliteDurableStore::open(
            data.join("store.db"),
        )?);
        let tenants = Arc::new(RwLock::new(HashMap::from([(
            prepared.manifest.name.clone(),
            prepared.tenant.clone(),
        )])));
        let directory = Arc::new(isoplane_runtime::directory::LocalDirectory::open(
            data.join("directory.jsonl"),
        )?);
        let alarms = Arc::new(isoplane_runtime::alarms::LocalAlarmService::open(
            data.join("alarms.jsonl"),
        )?);
        let registry = ActivationRegistry::new(
            ActivationConfig {
                cell: prepared.version.clone(),
                ..Default::default()
            },
            di_store.clone(),
            directory,
            Some(alarms.clone()),
            Arc::new(GatedSocketSink {
                sockets: sockets.clone(),
                tenants,
            }),
            Arc::new(|_: &DiId| Arc::new(UnusedGuest) as Arc<dyn DiGuest>),
            None,
        );
        let durable = DurableBridge::new(registry, di_store, alarms);
        let services = Arc::new(isoplane_host::cell::Services {
            store,
            durable: durable.clone(),
            leases: durable.clone(),
            sockets,
            egress: egress.clone(),
            fetcher: Arc::new(isoplane_host::PinnedHttpFetcher::new(
                egress.clone(),
                network.inline_response_bytes,
            )),
        });
        let mut config = CellConfig {
            name: prepared.manifest.name.clone(),
            ..Default::default()
        };
        if matches!(prepared.activator, PreparedActivator::Debug(_)) {
            config.wall_ms_cap = isoplane_host::debug::DEBUG_BUDGET_MS;
        }
        let snapshots = Arc::new(isoplane_hibernate::snapshot::LocalSnapshotStorage::new(
            scratch.path().join("snapshots"),
        )?);
        let cell = Cell::with_snapshot_storage(config, services, snapshots)?;
        let debug = match &prepared.activator {
            PreparedActivator::Debug(activator) => Some(activator.clone()),
            PreparedActivator::Ordinary(_) => None,
        };
        match prepared.activator {
            PreparedActivator::Ordinary(activator) => {
                cell.register_module(&prepared.tenant, activator, prepared.manifest.clone())?
            }
            PreparedActivator::Debug(activator) => {
                cell.register_debug_module(&prepared.tenant, activator, prepared.manifest.clone())?
            }
        }
        cell.set_runtime_configuration(
            &prepared.tenant,
            &ModuleId::new(&prepared.manifest.name, &prepared.version),
            configuration,
        )?;
        let target = RouteTarget {
            app: prepared.manifest.name,
            tenant: prepared.tenant,
            version: prepared.version,
            lifecycle: prepared.manifest.handlers.lifecycle,
            tier: isoplane_abi::host::CodeTier::Fast,
        };
        let routes = Arc::new(AppRoute(target.clone()));
        let proxy = isoplane_proxy::Proxy::with_hub(routes, cell.clone(), metrics, cfg, hub);
        let app = Arc::new(Self {
            cell,
            proxy,
            target,
            _scratch: scratch,
            debug,
        });
        durable.attach(
            app.cell.reactor().clone(),
            Arc::downgrade(&(app.clone() as Arc<dyn DurableCalls>)),
        )?;
        Ok(app)
    }
}

#[async_trait::async_trait]
impl DurableCalls for LocalApp {
    async fn call(
        &self,
        tenant: &str,
        req: isoplane_abi::calls::DiCall,
    ) -> Result<isoplane_abi::result::DiCallResponse, isoplane_abi::FutureError> {
        if tenant != self.target.tenant {
            return Err(isoplane_abi::FutureError::Denied(
                "the caller does not own this local app".into(),
            ));
        }
        let id = DiId {
            app: self.target.app.clone(),
            class: req.class,
            name: req.name,
        };
        let response = self
            .cell
            .dispatch_di(&self.target, &id, &req.method, req.payload)
            .await
            .map_err(|e| isoplane_abi::FutureError::Io(e.to_string()))?;
        Ok(isoplane_abi::result::DiCallResponse {
            payload: response.body,
        })
    }
}

struct AppRoute(RouteTarget);
impl isoplane_proxy::RouteResolver for AppRoute {
    fn resolve(&self, _host: &str, _path: &str, _key: Option<&str>) -> Option<RouteTarget> {
        Some(self.0.clone())
    }
    fn generation(&self) -> u64 {
        1
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn an_active_rebuild_does_not_pin_previous_compilation_entries() {
        let directory = tempfile::tempdir().unwrap();
        let library = directory.path().join("runtime");
        std::fs::write(&library, b"runtime").unwrap();
        let project = Project {
            manifest: serde_json::from_str(r#"{"name":"app"}"#).unwrap(),
            state_directory: directory.path().join("state"),
            runtime_library: library,
            compilation_directory: directory.path().join("compiled"),
            init_sandbox: isoplane_abi::config::InitSandbox::Sealed,
            allow_endpoints: vec![],
            platform_files: Default::default(),
            application_files: Default::default(),
        };
        let previous = compilation_cache(&project, b"previous").unwrap();
        let old = previous.lease.directory().to_path_buf();
        std::fs::write(old.join("artifact"), b"compiled").unwrap();
        let current = compilation_cache(&project, b"current").unwrap();
        std::fs::write(current.lease.directory().join("artifact"), b"compiled").unwrap();
        drop(previous);
        isoplane_cache::Cache::new(project.compilation_directory, isoplane_cache::CacheBytes(0))
            .unwrap()
            .prune()
            .unwrap();
        assert!(
            !old.exists(),
            "the active rebuild must not pin an obsolete artifact from the same runtime"
        );
        assert!(
            current.lease.directory().exists(),
            "retention must preserve the current rebuild's storage"
        );
    }
}
