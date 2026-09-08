//! One project's guest execution and local bindings, with no node control plane.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use wisp_abi::config::AppManifest;
use wisp_abi::host::{DiId, Dispatcher, DurableStore, RouteTarget};
use wisp_durable::activation::{ActivationConfig, ActivationRegistry, DiGuest};
use wisp_host::{Cell, CellConfig, EngineActivator, ModuleId};
use wisp_runtime::durable::{DurableBridge, DurableCalls, UnusedGuest};
use wisp_runtime::sockets::{GatedSocketSink, ProxySocketOps};

use super::project::Project;

pub struct Prepared {
    activator: Arc<EngineActivator>,
    manifest: AppManifest,
    tenant: String,
    version: String,
}

impl Prepared {
    pub fn build(project: &Project, wasm: &[u8]) -> anyhow::Result<Self> {
        let manifest = project.manifest.clone();
        let report = wisp_runtime::validate(wasm, manifest.limits).map_err(|violations| {
            anyhow::anyhow!(
                "module rejected:\n{}",
                violations
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })?;
        let compressed_bytes = zstd::encode_all(wasm, 19)?.len() as u64;
        let limit = manifest.limits.module_bytes_compressed;
        anyhow::ensure!(compressed_bytes <= limit, "module is {compressed_bytes} compressed bytes, exceeding the configured {limit} byte limit");
        let version = blake3::hash(wasm).to_hex().to_string();
        let template = if report.has_initialize() {
            wisp_runtime::PreInitializer::new()
                .with_pal(project.init_sandbox)
                .run(wasm, &wisp_runtime::PreInitRequest {
                    module_hash: wisp_engine::ModuleHash::of(wasm),
                    init_blob: serde_json::to_vec(&manifest.init_visible_vars)?,
                })
                .map_err(|e| anyhow::anyhow!("pre-initializing {} with {}: {e}; managed guests may require --init-sandbox runtime-bringup", manifest.name, project.init_sandbox.as_str()))?
                .template_wasm
        } else {
            wasm.to_vec()
        };
        let engine = Arc::new(wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
            pooling: false,
            max_memory_bytes: manifest.limits.instance_memory_max,
            ..Default::default()
        })?);
        let module = ModuleId::new(&manifest.name, &version);
        let activator = Arc::new(EngineActivator::load(engine, module, &template)?);
        Ok(Self {
            activator,
            manifest,
            tenant: project.tenant.clone(),
            version,
        })
    }
}

pub struct LocalApp {
    pub cell: Arc<Cell>,
    pub proxy: Arc<wisp_proxy::Proxy>,
    target: RouteTarget,
    _scratch: tempfile::TempDir,
}

impl LocalApp {
    pub async fn start(prepared: Prepared, data: &Path) -> anyhow::Result<Arc<Self>> {
        let network = wisp_runtime::network::NetworkLimits::default();
        let egress = Arc::new(wisp_host::HostEgressPolicy::system(
            network.connections,
            network.egress_bytes,
        )?);
        std::fs::create_dir_all(data)?;
        let scratch = tempfile::tempdir_in(data)?;
        let metrics = Arc::new(wisp_proxy::ProxyMetrics::new());
        let cfg = wisp_proxy::ProxyConfig::default();
        let hub = Arc::new(wisp_proxy::SocketHub::new(metrics.clone(), cfg.sockets));
        let sockets = ProxySocketOps::new(hub.clone());
        let store = Arc::new(wisp_store::SqliteStore::open(data.join("store.db"))?);
        let di_store: Arc<dyn DurableStore> =
            Arc::new(wisp_store::SqliteDurableStore::open(data.join("store.db"))?);
        let tenants = Arc::new(RwLock::new(HashMap::from([(
            prepared.manifest.name.clone(),
            prepared.tenant.clone(),
        )])));
        let directory = Arc::new(wisp_runtime::directory::LocalDirectory::open(
            data.join("directory.jsonl"),
        )?);
        let alarms = Arc::new(wisp_runtime::alarms::LocalAlarmService::open(
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
        let services = Arc::new(wisp_host::cell::Services {
            store,
            durable: durable.clone(),
            leases: durable.clone(),
            sockets,
            egress: egress.clone(),
            fetcher: Arc::new(wisp_host::PinnedHttpFetcher::new(
                egress.clone(),
                network.inline_response_bytes,
            )),
        });
        let cell = Cell::new(
            CellConfig {
                name: prepared.manifest.name.clone(),

                ..Default::default()
            },
            services,
        )?;
        cell.register_module(
            &prepared.tenant,
            prepared.activator,
            prepared.manifest.clone(),
        )?;
        let target = RouteTarget {
            app: prepared.manifest.name,
            tenant: prepared.tenant,
            version: prepared.version,

            lifecycle: prepared.manifest.handlers.lifecycle,
            tier: wisp_abi::host::CodeTier::Fast,
        };
        let routes = Arc::new(AppRoute(target.clone()));
        let proxy = wisp_proxy::Proxy::with_hub(routes, cell.clone(), metrics, cfg, hub);
        let app = Arc::new(Self {
            cell,
            proxy,
            target,
            _scratch: scratch,
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
        req: wisp_abi::calls::DiCall,
    ) -> Result<wisp_abi::result::DiCallResponse, wisp_abi::FutureError> {
        if tenant != self.target.tenant {
            return Err(wisp_abi::FutureError::Denied(
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
            .map_err(|e| wisp_abi::FutureError::Io(e.to_string()))?;
        Ok(wisp_abi::result::DiCallResponse {
            payload: response.body,
        })
    }
}

struct AppRoute(RouteTarget);
impl wisp_proxy::RouteResolver for AppRoute {
    fn resolve(&self, _host: &str, _path: &str, _key: Option<&str>) -> Option<RouteTarget> {
        Some(self.0.clone())
    }
    fn generation(&self) -> u64 {
        1
    }
}
