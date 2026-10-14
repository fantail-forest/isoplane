//! One project's guest execution and local bindings, with no node control plane.

mod baseline;
mod compressed_module;
mod preparation_cache;

pub use baseline::Baselines;

use std::path::Path;
use std::sync::Arc;

use isoplane_abi::config::AppManifest;
use isoplane_abi::host::RouteTarget;
use isoplane_host::{Cell, CellConfig, EngineActivator, ModuleId};

use super::Inputs as Project;

#[derive(Clone)]
enum PreparedActivator {
    Ordinary(Arc<EngineActivator>),
    Debug(Arc<isoplane_host::debug::DebugActivator>),
}

/// Validated guest code ready to register with local services.
#[derive(Clone)]
pub struct Prepared {
    activator: PreparedActivator,
    manifest: AppManifest,
    tenant: String,
    version: String,
}

impl Prepared {
    /// Validates and pre-initializes an ordinary local application. An
    /// application package is prepared over the runtime baseline `baselines`
    /// holds.
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
        let admit = || validate_runtime(project, wasm).map(drop);
        if let Some(prepared) = baselines.prepare(project, wasm, &settings, &version, admit) {
            return prepared;
        }
        let report = validate_runtime(project, wasm)?;
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
        })
    }
    /// Validates a marked debug module and compiles it, or reuses its
    /// compilation from the runtime cache, without pre-initialization.
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
        let engine = debug_engine(project)?;
        let (template, compiled) =
            runtime_cache(project, blake3::hash(wasm).as_bytes())?.load(&engine, wasm)?;
        tracing::info!(compiled, "local debug runtime compilation ready");
        let module = ModuleId::new(&manifest.name, &version);
        let activator = Arc::new(isoplane_host::debug::DebugActivator::new(
            engine, module, template,
        )?);
        Ok(Self {
            activator: PreparedActivator::Debug(activator),
            manifest,
            tenant: project.tenant(),
            version,
        })
    }
}

/// What preparing a shared runtime ahead of its first session did.
pub struct RuntimePreparation {
    /// Whether the runtime baseline was compiled, rather than found in the cache.
    pub baseline_compiled: bool,
    /// Whether the debug runtime was compiled, rather than found in the cache.
    pub debug_compiled: bool,
}

/// Compiles the shared runtime `wasm` and its debug build `debug_wasm` into
/// `project`'s runtime cache, as the first session and the first debug
/// session of an application package on them would.
pub fn prepare_runtime(
    project: &Project,
    wasm: &[u8],
    debug_wasm: &[u8],
) -> anyhow::Result<RuntimePreparation> {
    let baseline_compiled =
        baseline::prepare_runtime(project, wasm, || validate_runtime(project, wasm).map(drop))?;
    validate(project, debug_wasm, true)?;
    let engine = debug_engine(project)?;
    let (_, debug_compiled) =
        runtime_cache(project, blake3::hash(debug_wasm).as_bytes())?.load(&engine, debug_wasm)?;
    Ok(RuntimePreparation {
        baseline_compiled,
        debug_compiled,
    })
}

fn debug_engine(project: &Project) -> anyhow::Result<Arc<isoplane_engine::IsoplaneEngine>> {
    Ok(Arc::new(isoplane_engine::IsoplaneEngine::new(
        isoplane_engine::EngineConfig {
            pooling: false,
            max_memory_bytes: project.manifest.memory_max_bytes(),
            platform_files: project.served_files()?,
            ..Default::default()
        },
    )?))
}

fn engine_configuration(project: &Project) -> isoplane_engine::EngineConfig {
    isoplane_engine::EngineConfig {
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

/// Storage for what is compiled or prepared from this project alone.
fn compilation_cache(
    project: &Project,
    artifact_identity: &[u8],
) -> anyhow::Result<LocalCompilation> {
    let storage = isoplane_cache::Cache::new(
        project.compilation_directory.clone(),
        isoplane_cache::PROJECT_CACHE_BYTES,
    )?;
    local_compilation(project, storage, artifact_identity)
}

/// Storage for a compiled shared runtime, which holds nothing of the
/// project's and is reused by every project of the user on that runtime.
fn runtime_cache(project: &Project, runtime_identity: &[u8]) -> anyhow::Result<LocalCompilation> {
    let storage = isoplane_cache::Cache::new(
        project.runtime_cache_directory.clone(),
        isoplane_cache::RUNTIME_CACHE_BYTES,
    )?;
    local_compilation(project, storage, runtime_identity)
}

fn local_compilation(
    project: &Project,
    storage: isoplane_cache::Cache,
    artifact_identity: &[u8],
) -> anyhow::Result<LocalCompilation> {
    let mut identity = blake3::Hasher::new();
    identity.update_reader(std::fs::File::open(&project.runtime_library)?)?;
    identity.update(artifact_identity);
    let lease = Arc::new(storage.lease(isoplane_cache::CacheKey(*identity.finalize().as_bytes()))?);
    // SAFETY: only the compiler writes into this private, leased directory.
    let cache = unsafe {
        isoplane_engine::CompilationCache::from_trusted_directory(lease.directory().into())
    }?;
    Ok(LocalCompilation { cache, lease })
}

fn preinitialize(project: &Project, wasm: &[u8], configuration: &[u8]) -> anyhow::Result<Vec<u8>> {
    let files = project.served_files()?;
    Ok(isoplane_prepare::PreInitializer::new()
        .with_runtime_profile(project.runtime_profile.clone())
        .with_memory_limit(isoplane_abi::config::GuestMemoryBytes::new(
            project.manifest.memory_max_bytes(),
        )?)
        .with_engine_config(isoplane_engine::EngineConfig {
            platform_files: files.clone(),
            ..engine_configuration(project)
        })
        .with_network(initialization_network(project))
        .with_pal(project.init_sandbox)
        .with_runtime_configuration(configuration.to_vec())
        .with_platform_files(files)
        .run(
            wasm,
            &isoplane_prepare::PreInitRequest {
                module_hash: isoplane_engine::ModuleHash::of(wasm),
                init_blob: serde_json::to_vec(&project.manifest.init_visible_vars)?,
            },
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "pre-initializing {} with {}: {e}; ",
                project.manifest.name,
                project.init_sandbox.as_str()
            )
        })?
        .template_wasm)
}

fn initialization_network(project: &Project) -> isoplane_prepare::InitializationNetwork {
    let host = CellConfig::default();
    isoplane_prepare::InitializationNetwork {
        tenant: project.tenant(),
        allowlist: project.manifest.bindings.fetch_allowlist.clone(),
        destinations: isoplane_prepare::InitializationDestinations::LocalDevelopment,
        limits: isoplane_runtime::network::NetworkLimits::default(),
        native_handles: host.guest_resources.native_handles,
        pending_futures: project
            .manifest
            .effective_limits()
            .pending_futures_per_dispatch,
    }
}

fn validate_runtime(
    project: &Project,
    wasm: &[u8],
) -> anyhow::Result<isoplane_runtime::ValidationReport> {
    let report = validate(project, wasm, false)?;
    if !project.application_files.files().is_empty() {
        anyhow::ensure!(
            report.has_initialize()
                && report
                    .optional_exports
                    .contains(&isoplane_abi::imports::exports::INITIALIZE_RUNTIME),
            "the installed runtime requires both runtime and application initialization exports"
        );
    }
    Ok(report)
}

fn validate(
    project: &Project,
    wasm: &[u8],
    local_debug: bool,
) -> anyhow::Result<isoplane_runtime::ValidationReport> {
    let manifest = &project.manifest;
    let declaration = project
        .runtime_profile
        .declared_memory_ceiling(manifest.guest_memory_bytes);
    let admission = if local_debug {
        isoplane_runtime::validate_local_debug_with_limits(
            wasm,
            manifest.limits,
            declaration,
            &manifest.validation_limits,
        )
    } else {
        isoplane_runtime::validate_with_limits(
            wasm,
            manifest.limits,
            declaration,
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
    // A package is measured when it is read; the shared runtime it runs on is
    // the platform's, not part of the application's size.
    if project.application_files.files().is_empty() {
        let compressed_bytes = compressed_module::size(&project.state_directory, wasm)?;
        let limit = manifest.effective_limits().module_bytes_compressed;
        anyhow::ensure!(compressed_bytes <= limit, "module is {compressed_bytes} compressed bytes, exceeding the configured {limit} byte limit");
    }
    Ok(report)
}

/// A local application using the shared cell, proxy and persistent host services.
pub struct LocalApp {
    /// Owns guest dispatch and resource cleanup.
    pub cell: Arc<Cell>,
    /// Routes local HTTP through the shared request pipeline.
    pub proxy: Arc<isoplane_proxy::Proxy>,
    _scratch: tempfile::TempDir,
    debug: Option<Arc<isoplane_host::debug::DebugActivator>>,
}

impl LocalApp {
    /// Stops debugger waits, sockets and guest workers before releasing session state.
    pub fn shutdown(&self) {
        if let Some(debug) = &self.debug {
            debug.stop();
        }
        self.cell.shutdown();
    }

    /// Registers ordinary prepared code with local state.
    pub async fn start(
        prepared: Prepared,
        data: &Path,
        configuration: isoplane_host::bindings::RuntimeConfiguration,
    ) -> Result<Arc<Self>, super::Error> {
        Self::start_with(prepared, data, configuration)
            .await
            .map_err(super::Error::Startup)
    }

    async fn start_with(
        prepared: Prepared,
        data: &Path,
        configuration: isoplane_host::bindings::RuntimeConfiguration,
    ) -> anyhow::Result<Arc<Self>> {
        let network = isoplane_runtime::network::NetworkLimits::default();
        let limits = isoplane_host::egress::ConnectionLimits::new(
            isoplane_host::egress::ConnectionCeilings {
                in_progress: network.instance_pending_connections,
                open: network.instance_open_connections,
            },
        )?;
        let egress = isoplane_host::HostEgressPolicy::system(limits)?;
        let egress = egress.with_destinations(Arc::new(
            isoplane_host::egress::LocalDevelopmentDestinations,
        ));
        let egress = Arc::new(egress);
        std::fs::create_dir_all(data)?;
        let scratch = tempfile::tempdir_in(data)?;
        let metrics = Arc::new(isoplane_proxy::ProxyMetrics::new());
        let cfg = isoplane_proxy::ProxyConfig::default();
        let services = Arc::new(isoplane_host::cell::Services {
            egress: egress.clone(),
            fetcher: Arc::new(isoplane_host::PinnedHttpFetcher::new(
                network.inline_response_bytes,
            )),
        });
        let mut config = CellConfig {
            name: prepared.manifest.name.clone(),
            ..CellConfig::local()
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
        let proxy = isoplane_proxy::Proxy::new(routes, cell.clone(), metrics, cfg);
        let app = Arc::new(Self {
            cell,
            proxy,
            _scratch: scratch,
            debug,
        });
        Ok(app)
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

    fn project() -> (tempfile::TempDir, Project) {
        let directory = tempfile::tempdir().unwrap();
        let library = directory.path().join("runtime");
        std::fs::write(&library, b"runtime").unwrap();
        let project = Project {
            manifest: serde_json::from_str(r#"{"name":"app"}"#).unwrap(),
            state_directory: directory.path().join("state"),
            runtime_library: library,
            compilation_directory: directory.path().join("compiled"),
            runtime_cache_directory: directory.path().join("runtimes"),
            init_sandbox: isoplane_abi::config::InitSandbox::Sealed,
            platform_files: Default::default(),
            application_files: Default::default(),
            runtime_profile: isoplane_prepare::RuntimeProfile::raw_wasm(),
        };
        (directory, project)
    }

    #[test]
    fn an_active_rebuild_does_not_pin_previous_compilation_entries() {
        let (_directory, project) = project();
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

    #[test]
    fn shared_runtime_growth_obeys_local_application_limits_above_the_default() {
        use isoplane_engine::{Completion, InstanceOps, IsoplaneEngine, OccupancyBudget};
        use isoplane_host::bindings::{CellBindings, InstanceCtx, Tenancy};

        for manifest in [
            r#"{"name":"app","limits":"enterprise"}"#,
            r#"{"name":"app","guest_memory_bytes":167772160}"#,
        ] {
            let (_directory, mut project) = project();
            project.manifest = serde_json::from_str(manifest).unwrap();
            project.runtime_profile = isoplane_prepare::RuntimeProfile::SharedRuntime {
                sdk_version: "test-sdk".into(),
            };
            let limit = project.manifest.memory_max_bytes();
            let wasm = wat::parse_str(format!(
                r#"(module
                (memory (export "memory") 1 65536)
                (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
                (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 1)
                (func (export "isoplane_initialize") (result i64)
                    (if (i32.ne (memory.grow (i32.const 2048)) (i32.const 1))
                        (then unreachable))
                    (if (i32.ne (memory.grow (i32.const {over_limit_pages})) (i32.const -1))
                        (then unreachable))
                    i64.const 0)
                (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 1))"#,
                over_limit_pages = limit / isoplane_abi::WASM_PAGE_SIZE as u64 - 2048,
            ))
            .unwrap();
            validate(&project, &wasm, false).unwrap();
            let engine = IsoplaneEngine::new(isoplane_engine::EngineConfig {
                max_instances: 1,
                pooling: false,
                ..engine_configuration(&project)
            })
            .unwrap();
            let template = engine.load_template(&wasm).unwrap();
            let mut context = InstanceCtx::new(
                isoplane_host::InstanceId(1),
                Tenancy {
                    module: ModuleId::new("app", "test"),
                    tenant: "test".into(),
                    limits: project.manifest.limits,
                    lifecycle: Default::default(),
                    allowlist: None,
                },
                Arc::default(),
                None,
            );
            context.limits = project.manifest.effective_limits();
            let mut instance = engine
                .activate(&template, CellBindings::boxed(Arc::new(context)))
                .unwrap();
            let outcome = instance.run_initialize(OccupancyBudget::http());
            assert_eq!(outcome.completion, Completion::Done, "{outcome:?}");
            assert_eq!(instance.memory().len(), 2049 * isoplane_abi::WASM_PAGE_SIZE);
            project.runtime_profile = isoplane_prepare::RuntimeProfile::raw_wasm();
            assert!(
                validate(&project, &wasm, false).is_err(),
                "raw modules cannot claim the shared runtime's declaration ceiling"
            );
        }
    }
}

#[cfg(feature = "diagnostic-benchmark")]
mod concurrent_pairs;
#[cfg(feature = "diagnostic-benchmark")]
pub use concurrent_pairs::run_concurrent_pairs;
