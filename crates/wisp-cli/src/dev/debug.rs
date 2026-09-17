//! One local request through a fresh debug guest and the ordinary host services.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use super::project::Project;
use super::runtime::{LocalApp, Prepared};

pub use isoplane_native::local::debug::DebugPeer;

/// Serves one connection/request from raw module bytes, without pre-initialization.
pub async fn run(
    style: &wisp_cli::render::Style,
    directory: &Path,
    module: &Path,
    endpoint: SocketAddr,
    port: u16,
    json: bool,
) -> anyhow::Result<()> {
    let peer = DebugPeer::new(endpoint)?;
    let mut project = Project::load(directory, None, None, true)?;
    project.module = module
        .canonicalize()
        .context("opening the debug session module")?;
    let mut lock = super::project_lock(&project.root)?;
    let _owner = lock
        .try_write()
        .context("another local development session owns this project")?;
    let wasm = std::fs::read(&project.module)?;
    let build_project = project.clone();
    let prepared = tokio::task::spawn_blocking(move || -> anyhow::Result<Prepared> {
        Ok(Prepared::build_debug(
            &build_project.native_inputs()?,
            &wasm,
        )?)
    })
    .await??;
    let session = project
        .module
        .parent()
        .context("the debug module must belong to a session directory")?;
    let app = LocalApp::start_debug(prepared, &session.join("runtime"), peer).await?;
    let result = serve(style, &project, app.clone(), port, json).await;
    app.proxy.shutdown_sockets();
    tokio::task::spawn_blocking(move || app.shutdown()).await?;
    result
}

async fn serve(
    style: &wisp_cli::render::Style,
    project: &Project,
    app: Arc<LocalApp>,
    port: u16,
    json: bool,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    super::report(
        style,
        json,
        "ready",
        &project.manifest.name,
        listener.local_addr()?,
    )?;
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            connection = listener.accept() => {
                let (stream, peer) = connection?;
                connections.spawn(serve_connection(stream, peer, app.clone(), admitted.clone()));
            }
            completed = connections.join_next(), if !connections.is_empty() => {
                if let Some(completed) = completed {
                    let (handled, result) = completed?;
                    if handled {
                        super::stop_connections(&mut connections).await;
                        return result;
                    }
                    if let Err(error) = result { tracing::debug!(%error, "empty debug connection closed"); }
                }
            }
            stop = tokio::signal::ctrl_c() => { stop?; break; }
        }
    }
    super::stop_connections(&mut connections).await;
    Ok(())
}

async fn serve_connection<S>(
    stream: S,
    peer: SocketAddr,
    app: Arc<LocalApp>,
    admitted: Arc<std::sync::atomic::AtomicBool>,
) -> (bool, anyhow::Result<()>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    let handled = Arc::new(AtomicBool::new(false));
    let observed = handled.clone();
    let service = service_fn(move |request| {
        let app = app.clone();
        let handled = handled.clone();
        let admitted = admitted.clone();
        async move {
            if admitted.swap(true, Ordering::AcqRel) {
                let mut response = hyper::Response::new(wisp_proxy::server::Body::default());
                *response.status_mut() = hyper::StatusCode::SERVICE_UNAVAILABLE;
                return Ok(response);
            }
            handled.store(true, Ordering::Release);
            app.proxy.clone().handle(request, peer).await
        }
    });
    let result = hyper::server::conn::http1::Builder::new()
        .keep_alive(false)
        .serve_connection(TokioIo::new(stream), service)
        .await
        .context("serving the debug session's HTTP request");
    (observed.load(Ordering::Acquire), result)
}
