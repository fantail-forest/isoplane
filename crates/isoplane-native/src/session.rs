use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Context;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use serde::Deserialize;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::local::{debug::DebugPeer, Inputs, LocalApp, Prepared};

pub(crate) struct Session {
    pub cancellation: CancellationToken,
    pending: Mutex<Option<Update>>,
    changed: Notify,
    started: AtomicBool,
}

struct Update {
    wasm: Vec<u8>,
    options: Options,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Options {
    manifest: wisp_abi::config::AppManifest,
    state_directory: PathBuf,
    runtime_library: PathBuf,
    #[serde(default)]
    init_sandbox: wisp_abi::config::InitSandbox,
    #[serde(default)]
    allow_endpoints: Vec<SocketAddr>,
    #[serde(default)]
    trust: wisp_trust::TrustConfig,
    #[serde(default)]
    configuration: std::collections::BTreeMap<String, String>,
    log_filter: Option<String>,
    port: Option<u16>,
    debug_endpoint: Option<SocketAddr>,
}

impl Options {
    fn inputs(&self) -> anyhow::Result<Inputs> {
        anyhow::ensure!(
            !self.manifest.name.trim().is_empty(),
            "local application name is empty"
        );
        anyhow::ensure!(
            self.state_directory.is_absolute(),
            "local state directory must be absolute"
        );
        anyhow::ensure!(
            self.allow_endpoints
                .iter()
                .all(|address| address.port() != 0
                    && !address.ip().is_unspecified()
                    && !address.ip().is_multicast()),
            "development services must name individual IP addresses and nonzero ports"
        );
        let trust = wisp_trust::TrustBundle::load(&self.trust)?;
        let platform_files = wisp_engine::PlatformFiles::new(vec![wisp_engine::PlatformFile {
            path: wisp_trust::GUEST_PATH.into(),
            bytes: trust.pem().into(),
            modified_ns: trust.modified_ns(),
        }])?;
        let identity = blake3::hash(self.state_directory.as_os_str().as_encoded_bytes());
        Ok(Inputs {
            manifest: self.manifest.clone(),
            tenant: format!("local:{identity}"),
            state_directory: self.state_directory.clone(),
            runtime_library: self.runtime_library.clone(),
            init_sandbox: self.init_sandbox,
            allow_endpoints: self.allow_endpoints.clone(),
            platform_files,
        })
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            cancellation: CancellationToken::new(),
            pending: Mutex::new(None),
            changed: Notify::new(),
            started: AtomicBool::new(false),
        }
    }

    pub fn reload(&self, wasm: &[u8], options: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.cancellation.is_cancelled(),
            "local session has stopped"
        );
        let update = Update {
            wasm: wasm.to_vec(),
            options: serde_json::from_slice(options)?,
        };
        *self.pending.lock() = Some(update);
        self.changed.notify_one();
        Ok(())
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let subscriber = events.subscriber(options.log_filter.as_deref().unwrap_or("info"))?;
        tracing::dispatcher::with_default(&subscriber, || {
            runtime.block_on(self.serve(wasm, options, &events))
        })
    }

    async fn serve(
        &self,
        wasm: &[u8],
        options: Options,
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
            options.port.unwrap_or(wisp_abi::config::DEFAULT_PROXY_PORT),
        ))
        .await?;
        let address = listener.local_addr()?;
        let prepared = prepare(wasm.to_vec(), &options, inputs).await?;
        let mut app = prepared.start().await?;
        let mut connections = JoinSet::new();
        let debug = options.debug_endpoint.is_some();
        let admitted = Arc::new(AtomicBool::new(false));
        let result = async {
            if self.cancellation.is_cancelled() { return Ok(()); }
            events.emit(serde_json::json!({"event":"ready", "app":options.manifest.name, "url":format!("http://{address}")}));
            loop {
                tokio::select! {
                    biased;
                    _ = self.cancellation.cancelled() => break,
                    _ = self.changed.notified(), if !debug => {
                        let Some(update) = self.pending.lock().take() else { continue };
                        let replacement = async {
                        anyhow::ensure!(update.options.state_directory == options.state_directory
                            && update.options.manifest.name == options.manifest.name
                            && update.options.debug_endpoint.is_none(), "reload cannot change local session identity or debugger mode");
                            let inputs = update.options.inputs()?;
                            prepare(update.wasm, &update.options, inputs).await
                        }.await;
                        match replacement {
                            Ok(next) => {
                                stop_connections(&mut connections).await;
                                shutdown(app.clone()).await?;
                                app = next.start().await?;
                                events.emit(serde_json::json!({"event":"reloaded", "app":options.manifest.name, "url":format!("http://{address}")}));
                            }
                            Err(error) => events.emit(serde_json::json!({"event":"reload_failed", "detail":format!("{error:#}")})),
                        }
                    }
                    connection = listener.accept() => {
                        let (stream, peer) = connection?;
                        connections.spawn(connection_task(stream, peer, app.clone(), debug, admitted.clone()));
                    }
                    completed = connections.join_next(), if !connections.is_empty() => {
                        if let Some(completed) = completed {
                            match completed {
                                Ok(handled) if debug && handled => break,
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
    configuration: wisp_host::bindings::RuntimeConfiguration,
    debug: Option<DebugPeer>,
}

impl PendingApp {
    async fn start(self) -> anyhow::Result<Arc<LocalApp>> {
        match self.debug {
            Some(peer) => Ok(LocalApp::start_debug(self.prepared, &self.directory, peer).await?),
            None => Ok(
                LocalApp::start(self.prepared, &self.directory, self.configuration).await?,
            ),
        }
    }
}

async fn prepare(wasm: Vec<u8>, options: &Options, inputs: Inputs) -> anyhow::Result<PendingApp> {
    let configuration = wisp_host::bindings::RuntimeConfiguration::from_json(&serde_json::to_vec(
        &options.configuration,
    )?)?;
    let preparation_configuration = configuration.clone();
    let debug = options.debug_endpoint.map(DebugPeer::new).transpose()?;
    let directory = inputs.state_directory.clone();
    let subscriber = tracing::dispatcher::get_default(Clone::clone);
    let prepared = tokio::task::spawn_blocking(move || {
        tracing::dispatcher::with_default(&subscriber, || {
            if debug.is_some() {
                Prepared::build_debug(&inputs, &wasm)
            } else {
                Prepared::build(&inputs, &wasm, &preparation_configuration)
            }
        })
    })
    .await??;
    Ok(PendingApp {
        prepared,
        directory,
        configuration,
        debug,
    })
}

async fn shutdown(app: Arc<LocalApp>) -> anyhow::Result<()> {
    app.proxy.shutdown_sockets();
    tokio::task::spawn_blocking(move || app.shutdown()).await?;
    Ok(())
}

async fn stop_connections(connections: &mut JoinSet<bool>) {
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
    app: Arc<LocalApp>,
    debug: bool,
    admitted: Arc<AtomicBool>,
) -> bool {
    let handled = Arc::new(AtomicBool::new(false));
    let observed = handled.clone();
    let service = service_fn(move |request| {
        let app = app.clone();
        let admitted = admitted.clone();
        let handled = handled.clone();
        async move {
            if debug && admitted.swap(true, Ordering::AcqRel) {
                let mut response = hyper::Response::new(wisp_proxy::server::Body::default());
                *response.status_mut() = hyper::StatusCode::SERVICE_UNAVAILABLE;
                return Ok(response);
            }
            handled.store(true, Ordering::Release);
            app.proxy.clone().handle(request, peer).await
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
    observed.load(Ordering::Acquire)
}
