use isoplane_abi::config::{AppManifest, InitSandbox};
use isoplaned::node::Node;

#[allow(
    dead_code,
    reason = "shared fixture results are inspected by different integration binaries"
)]
pub struct Deployed {
    pub initialized: isoplane_prepare::Initialized,
    pub prepared: isoplaned::deploy::PreparedReceipt,
    pub stages: Vec<isoplaned::node::DeployStage>,
}

#[derive(Clone)]
pub struct Guest {
    pub wasm: Vec<u8>,
    pub files: isoplane_engine::PlatformFiles,
    pub runtime: isoplane_prepare::RuntimeProfile,
}

impl From<Vec<u8>> for Guest {
    fn from(wasm: Vec<u8>) -> Self {
        Self {
            wasm,
            files: Default::default(),
            runtime: isoplane_prepare::RuntimeProfile::RawWasm,
        }
    }
}

impl From<&str> for Guest {
    fn from(name: &str) -> Self {
        let files = isoplane_fixtures::managed_probe(name)
            .into_iter()
            .map(|file| isoplane_engine::PlatformFile {
                path: file.path,
                bytes: file.bytes.into(),
                modified_ns: 0,
            })
            .collect();
        Self {
            wasm: isoplane_fixtures::mono_runtime(),
            runtime: isoplane_prepare::RuntimeProfile::SharedRuntime {
                sdk_version: isoplane_fixtures::release_version(),
            },
            files: isoplane_engine::PlatformFiles::new(files).expect("valid managed fixture files"),
        }
    }
}

impl From<&Vec<u8>> for Guest {
    fn from(wasm: &Vec<u8>) -> Self {
        Self::from(wasm.clone())
    }
}
impl From<&[u8]> for Guest {
    fn from(wasm: &[u8]) -> Self {
        Self::from(wasm.to_vec())
    }
}
impl From<&Guest> for Guest {
    fn from(guest: &Guest) -> Self {
        guest.clone()
    }
}

pub(crate) trait LocalDeployment {
    async fn deploy_locally(
        &self,
        app: &str,
        version: &str,
        tenant: &str,
        manifest: AppManifest,
        guest: impl Into<Guest>,
        sandbox: InitSandbox,
    ) -> anyhow::Result<Deployed>;
}

/// Local preparation compiling into the artifact store of a node configured as `cfg`.
pub fn provider(cfg: &isoplaned::config::NodeConfig) -> isoplane_prepare::LocalPreparation {
    let artifacts = isoplaned::real::RealPaths::under(&cfg.data_dir).artifacts;
    // SAFETY: the node under test owns this cache, and only the compiler's atomic,
    // content-addressed writer fills it.
    let cache = unsafe { isoplane_engine::CompilationCache::from_trusted_directory(artifacts) }
        .expect("the node's compilation cache");
    isoplane_prepare::LocalPreparation::with_cache(std::sync::Arc::new(cache))
        .expect("local preparation")
}

/// Publishes the hosts `node` configures for `app` to its registered `version`.
pub fn route(node: &Node, app: &str, version: &str) {
    let hosts = &node
        .cfg
        .apps
        .iter()
        .find(|configured| configured.manifest.name == app)
        .expect("a routed application is configured")
        .hosts;
    node.routes
        .insert(
            isoplane_abi::host::RouteTarget {
                app: app.to_owned(),
                tenant: node.cfg.owner.clone(),
                version: version.to_owned(),
                tier: isoplane_abi::host::CodeTier::Optimized,
            },
            hosts,
        )
        .expect("fixture host routes");
}

impl LocalDeployment for Node {
    async fn deploy_locally(
        &self,
        app: &str,
        version: &str,
        tenant: &str,
        mut manifest: AppManifest,
        guest: impl Into<Guest>,
        sandbox: InitSandbox,
    ) -> anyhow::Result<Deployed> {
        let guest = guest.into();
        let provider = provider(&self.cfg);
        let engine = self.real.engine();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let context = isoplaned::deploy::PreparationContext {
            network: network(tenant, &manifest),
            runtime_configuration: Default::default(),
            application_files: guest.files.clone(),
            engine,
            provider: &provider,
            runtime: guest.runtime.clone(),
            occupancy_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_OCCUPANCY_MS,
            wall_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
            max_passes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_PASSES,
            log_bytes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
            cancellation: &cancellation,
        };
        manifest.name = app.to_string();
        let prepared = self
            .real
            .deployer
            .prepare_application(
                &manifest,
                &guest.wasm,
                &isoplane_control::DeployOptions {
                    version: isoplane_control::VersionId(version.to_string()),
                    tenant: tenant.to_string(),
                    init_sandbox: sandbox,
                },
                &context,
            )
            .await?;
        let (installed, initialized, stages) = prepared.into_application();
        let receipt = installed.receipt.clone();
        self.real.install_application(tenant, installed).await?;
        route(self, app, version);
        Ok(Deployed {
            prepared: receipt,
            stages,
            initialized,
        })
    }
}

/// The same default resource policy as local execution, with the fixture's owner.
pub fn network(tenant: &str, manifest: &AppManifest) -> isoplane_prepare::InitializationNetwork {
    let host = isoplane_host::CellConfig::default();
    isoplane_prepare::InitializationNetwork {
        tenant: tenant.to_owned(),
        allowlist: manifest.bindings.fetch_allowlist.clone(),
        destinations: isoplane_prepare::InitializationDestinations::Public {
            exceptions: Vec::new(),
        },
        limits: Default::default(),
        native_handles: host.guest_resources.native_handles,
        open_connections: manifest.effective_limits().instance_open_connections,
        pending_futures: manifest.effective_limits().pending_futures_per_dispatch,
        buffered_bytes: host.buffered_future_bytes,
    }
}
