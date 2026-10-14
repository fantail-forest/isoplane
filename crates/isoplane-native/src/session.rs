use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use parking_lot::{Mutex, RwLock};
use serde::Deserialize;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::local::{Baselines, Inputs, LocalApp, Prepared};

thread_local! {
    static SESSION_SUBSCRIBER: std::cell::RefCell<Option<tracing::dispatcher::DefaultGuard>> = const { std::cell::RefCell::new(None) };
}

pub(crate) struct Session {
    pub cancellation: CancellationToken,
    baselines: Baselines,
    pending: Mutex<Option<Update>>,
    admission: Arc<Mutex<Admission>>,
    changed: Notify,
    started: AtomicBool,
}

#[derive(Default)]
struct Admission {
    debug: bool,
    active: bool,
    queued: bool,
}

struct Update {
    wasm: Vec<u8>,
    options: Options,
    fallback: Option<(Vec<u8>, Options)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Options {
    manifest: isoplane_abi::config::AppManifest,
    state_directory: PathBuf,
    runtime_library: PathBuf,
    compilation_directory: PathBuf,
    runtime_cache_directory: PathBuf,
    /// The developer machine's trusted roots, exported by the CLI as PEM.
    machine_roots: Option<PathBuf>,
    #[serde(default)]
    pub(crate) configuration: std::collections::BTreeMap<String, String>,
    log_filter: Option<String>,
    port: Option<u16>,
    debug_endpoint: Option<SocketAddr>,
    /// The application package loaded by the shared runtime.
    package: PathBuf,
}

impl Options {
    pub(crate) fn inputs(&self) -> anyhow::Result<Inputs> {
        anyhow::ensure!(
            !self.manifest.name.trim().is_empty(),
            "local application name is empty"
        );
        anyhow::ensure!(
            self.state_directory.is_absolute(),
            "local state directory must be absolute"
        );
        anyhow::ensure!(
            self.compilation_directory.is_absolute(),
            "local compiler storage must be absolute"
        );
        anyhow::ensure!(
            self.runtime_cache_directory.is_absolute(),
            "local runtime cache must be absolute"
        );
        anyhow::ensure!(
            self.package.is_absolute(),
            "the application package path must be absolute"
        );
        let archive = std::fs::read(&self.package).with_context(|| {
            format!("reading the application package {}", self.package.display())
        })?;
        let package = isoplane_prepare::package::read(
            &archive,
            isoplane_prepare::package::PackageLimits::of(&self.manifest),
        )
        .with_context(|| format!("serving the application package {}", self.package.display()))?;
        let mut files = vec![trust_file(self.machine_roots.as_deref())?];
        if let Some(endpoint) = self.debug_endpoint {
            files.push(isoplane_engine::PlatformFile {
                path: isoplane_abi::filesystem::DEBUG_AGENT_PATH.into(),
                bytes: endpoint.to_string().into_bytes().into(),
                modified_ns: 0,
            });
        }
        let platform_files = isoplane_engine::PlatformFiles::new(files)?;
        Ok(Inputs {
            manifest: self.manifest.clone(),
            state_directory: self.state_directory.clone(),
            runtime_library: self.runtime_library.clone(),
            compilation_directory: self.compilation_directory.clone(),
            runtime_cache_directory: self.runtime_cache_directory.clone(),
            init_sandbox: RUNTIME_SANDBOX,
            platform_files,
            application_files: package.files,
            runtime_profile: isoplane_prepare::RuntimeProfile::SharedRuntime {
                sdk_version: package.runtime.sdk_version,
            },
        })
    }
}

/// An application package runs on the shared managed runtime, which reads the
/// clock and draws entropy while it comes up.
pub(crate) const RUNTIME_SANDBOX: isoplane_abi::config::InitSandbox =
    isoplane_abi::config::InitSandbox::RuntimeBringUp;

/// The trusted roots a guest reads: the machine's exported roots joined to the
/// shipped ones, or the shipped ones alone.
pub(crate) fn trust_file(
    machine_roots: Option<&std::path::Path>,
) -> anyhow::Result<isoplane_engine::PlatformFile> {
    let trust = match machine_roots {
        Some(path) => {
            let machine = std::fs::read(path).with_context(|| {
                format!("reading the machine's trusted roots {}", path.display())
            })?;
            isoplane_trust::TrustBundle::local_development(&machine)?
        }
        None => isoplane_trust::TrustBundle::load(&isoplane_trust::TrustConfig::default())?,
    };
    Ok(isoplane_engine::PlatformFile {
        path: isoplane_trust::GUEST_PATH.into(),
        bytes: trust.pem().into(),
        modified_ns: trust.modified_ns(),
    })
}

impl Session {
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            pending: Mutex::new(None),
            admission: Arc::new(Mutex::new(Admission::default())),
            changed: Notify::new(),
            started: AtomicBool::new(false),
            baselines: Baselines::default(),
        }
    }

    pub fn reload(&self, wasm: &[u8], options: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.cancellation.is_cancelled(),
            "local session has stopped"
        );
        let mut update = Update {
            wasm: wasm.to_vec(),
            options: serde_json::from_slice(options)?,
            fallback: None,
        };
        let mut admission = self.admission.lock();
        admission.queued = true;
        let mut pending = self.pending.lock();
        if admission.debug && update.options.debug_endpoint.is_none() {
            update.fallback = pending.take().and_then(|previous| {
                if previous.options.debug_endpoint.is_some() {
                    Some((previous.wasm, previous.options))
                } else {
                    previous.fallback
                }
            });
        }
        *pending = Some(update);
        drop(pending);
        drop(admission);
        self.changed.notify_one();
        Ok(())
    }

    fn can_prepare_update(&self) -> bool {
        let admission = self.admission.lock();
        self.pending
            .lock()
            .as_ref()
            .is_some_and(|update| !admission.active || update.options.debug_endpoint.is_none())
    }

    pub fn run(
        &self,
        wasm: &[u8],
        options: &[u8],
        events: crate::session_events::Events,
    ) -> anyhow::Result<()> {
        let _events_lifetime = events.lifetime();
        let options: Options = serde_json::from_slice(options)?;
        anyhow::ensure!(
            !self.started.swap(true, Ordering::AcqRel),
            "local session was already started"
        );
        let subscriber = events.subscriber(options.log_filter.as_deref().unwrap_or("info"))?;
        let worker_subscriber = subscriber.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(isoplane_host::threads::ThreadAllocation::local().outer)
            .on_thread_start(move || {
                SESSION_SUBSCRIBER.with_borrow_mut(|guard| {
                    *guard = Some(tracing::dispatcher::set_default(&worker_subscriber));
                });
            })
            .on_thread_stop(|| {
                SESSION_SUBSCRIBER.with_borrow_mut(|guard| drop(guard.take()));
            })
            .enable_all()
            .build()?;
        tracing::dispatcher::with_default(&subscriber, || {
            runtime.block_on(self.serve(wasm, options, &events))
        })
    }

    async fn serve(
        &self,
        wasm: &[u8],
        mut options: Options,
        events: &crate::session_events::Events,
    ) -> anyhow::Result<()> {
        let inputs = options.inputs()?;
        std::fs::create_dir_all(&inputs.state_directory)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(inputs.state_directory.join("dev.lock"))?;
        let mut lock = fd_lock::RwLock::new(file);
        let _owner = lock
            .try_write()
            .context("another local development session owns this application")?;
        let listener = tokio::net::TcpListener::bind((
            Ipv4Addr::LOCALHOST,
            options
                .port
                .unwrap_or(isoplane_abi::config::DEFAULT_PROXY_PORT),
        ))
        .await?;
        let address = listener.local_addr()?;
        let prepared = prepare(wasm.to_vec(), &options, inputs, &self.baselines).await?;
        let mut app = prepared.start().await?;
        let current_proxy = Arc::new(RwLock::new(app.proxy.clone()));
        let mut connections = JoinSet::new();
        self.admission.lock().debug = options.debug_endpoint.is_some();
        let mut prepared_update: Option<(PendingApp, Options)> = None;
        let result = async {
            if self.cancellation.is_cancelled() { return Ok(()); }
            events.emit(serde_json::json!({"event":"ready", "app":options.manifest.name, "url":format!("http://{address}")}));
            loop {
                tokio::select! {
                    biased;
                    _ = self.cancellation.cancelled() => break,
                    _ = self.changed.notified() => {},
                    _ = std::future::ready(()), if self.can_prepare_update() => {
                        let update = {
                            let admission = self.admission.lock();
                            let mut pending = self.pending.lock();
                            if admission.active && pending.as_ref().is_some_and(|update| update.options.debug_endpoint.is_some()) {
                                continue;
                            }
                            pending.take()
                        };
                        let Some(update) = update else { continue; };
                        let detaching = options.debug_endpoint.is_some() && update.options.debug_endpoint.is_none();
                        let replacement = async {
                            anyhow::ensure!(update.options.state_directory == options.state_directory
                                && update.options.manifest.name == options.manifest.name
                                && (update.options.debug_endpoint == options.debug_endpoint || detaching),
                                "reload cannot change local session identity or enable a debugger");
                            let inputs = update.options.inputs()?;
                            prepare(update.wasm, &update.options, inputs, &self.baselines).await
                        };
                        let replacement = tokio::select! {
                            _ = self.cancellation.cancelled() => break,
                            replacement = replacement => replacement,
                        };
                        match replacement {
                            Ok(next) => {
                                prepared_update = Some((next, update.options));
                                if detaching {
                                    events.emit(serde_json::json!({"event":"debugger_detach_prepared"}));
                                }
                            }
                            Err(error) => {
                                let mut admission = self.admission.lock();
                                let mut pending = self.pending.lock();
                                if pending.is_none() {
                                    *pending = update.fallback.map(|(wasm, options)| Update { wasm, options, fallback: None });
                                }
                                admission.queued = prepared_update.is_some() || pending.is_some();
                                drop(pending);
                                drop(admission);
                                events.emit(serde_json::json!({"event":if detaching { "debugger_detach_failed" } else { "reload_failed" }, "detail":format!("{error:#}")}));
                            }
                        }
                    }
                    _ = std::future::ready(()), if prepared_update.is_some() && !self.admission.lock().active => {
                        let Some((next, next_options)) = prepared_update.take() else { continue; };
                        shutdown(app.clone()).await?;
                        app = next.start().await?;
                        let detached = options.debug_endpoint.is_some() && next_options.debug_endpoint.is_none();
                        options = next_options;
                        {
                            let mut admission = self.admission.lock();
                            *current_proxy.write() = app.proxy.clone();
                            admission.debug = options.debug_endpoint.is_some();
                            admission.queued = self.pending.lock().is_some();
                        }
                        events.emit(serde_json::json!({"event":if detached { "debugger_detached" } else { "reloaded" }, "app":options.manifest.name, "url":format!("http://{address}")}));
                    }
                    connection = listener.accept() => {
                        let (stream, peer) = connection?;
                        let debug = self.admission.lock().debug;
                        connections.spawn(connection_task(stream, peer, current_proxy.clone(), debug, self.admission.clone()));
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        if let Some(completed) = completed {
                            match completed {
                                Ok(_) => {},
                                Err(error) => tracing::warn!(%error, "local connection task failed"),
                            }
                        }
                    }
                }
            }
            Ok(())
        }.await;
        self.cancellation.cancel();
        stop_connections(&mut connections).await;
        shutdown(app).await?;
        result
    }
}

struct PendingApp {
    prepared: Prepared,
    directory: PathBuf,
    configuration: isoplane_host::bindings::RuntimeConfiguration,
}

impl PendingApp {
    async fn start(self) -> anyhow::Result<Arc<LocalApp>> {
        Ok(LocalApp::start(self.prepared, &self.directory, self.configuration).await?)
    }
}

async fn prepare(
    wasm: Vec<u8>,
    options: &Options,
    inputs: Inputs,
    baselines: &Baselines,
) -> anyhow::Result<PendingApp> {
    let baselines = baselines.clone();
    let configuration = isoplane_host::bindings::RuntimeConfiguration::from_json(
        &serde_json::to_vec(&options.configuration)?,
    )?;
    let preparation_configuration = configuration.clone();
    let debug = options.debug_endpoint.is_some();
    if let Some(endpoint) = options.debug_endpoint {
        crate::local::debug::require_loopback_endpoint(endpoint)?;
    }
    let directory = inputs.state_directory.clone();
    let subscriber = tracing::dispatcher::get_default(Clone::clone);
    let prepared = tokio::task::spawn_blocking(move || {
        tracing::dispatcher::with_default(&subscriber, || {
            if debug {
                Prepared::build_debug(&inputs, &wasm)
            } else {
                Prepared::build(&inputs, &wasm, &preparation_configuration, &baselines)
            }
        })
    })
    .await??;
    Ok(PendingApp {
        prepared,
        directory,
        configuration,
    })
}

async fn shutdown(app: Arc<LocalApp>) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || app.shutdown()).await?;
    Ok(())
}

async fn stop_connections(connections: &mut JoinSet<()>) {
    connections.abort_all();
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            if !error.is_cancelled() {
                tracing::warn!(%error, "local connection failed during shutdown");
            }
        }
    }
}

async fn connection_task(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    current_proxy: Arc<RwLock<Arc<isoplane_proxy::Proxy>>>,
    debug: bool,
    admission: Arc<Mutex<Admission>>,
) {
    let handled = Arc::new(AtomicBool::new(false));
    let observed = handled.clone();
    let completed = admission.clone();
    let service = service_fn(move |request| {
        let proxy = {
            let mut admission = admission.lock();
            if admission.debug && (admission.active || admission.queued) {
                None
            } else {
                if admission.debug {
                    admission.active = true;
                    handled.store(true, Ordering::Release);
                }
                Some(current_proxy.read().clone())
            }
        };
        async move {
            match proxy {
                Some(proxy) => proxy.handle(request, peer).await,
                None => {
                    let mut response =
                        hyper::Response::new(isoplane_proxy::server::Body::default());
                    *response.status_mut() = hyper::StatusCode::SERVICE_UNAVAILABLE;
                    Ok(response)
                }
            }
        }
    });
    if let Err(error) = hyper::server::conn::http1::Builder::new()
        .keep_alive(!debug)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await
    {
        tracing::debug!(%error, "local connection closed");
    }
    if observed.load(Ordering::Acquire) {
        completed.lock().active = false;
    }
}
