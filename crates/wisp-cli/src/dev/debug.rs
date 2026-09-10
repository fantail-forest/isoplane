//! One local request through a fresh debug guest and the ordinary host services.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use wisp_host::egress::{DenyReason, DestinationPolicy, PublicDestinations};

use super::project::Project;
use super::runtime::{LocalApp, Prepared};

/// The one session-owned debugger destination allowed alongside public egress.
#[derive(Clone, Copy)]
pub struct DebugPeer(SocketAddr);

impl DebugPeer {
    /// Refuses non-loopback peers and unbound port zero.
    pub fn new(address: SocketAddr) -> anyhow::Result<Self> {
        anyhow::ensure!(
            address.ip() == IpAddr::V4(Ipv4Addr::LOCALHOST) && address.port() != 0,
            "the debugger endpoint must be the session-owned 127.0.0.1 address with a bound port"
        );
        Ok(Self(address))
    }
}

impl DestinationPolicy for DebugPeer {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if address == self.0 {
            Ok(())
        } else {
            PublicDestinations.authorize(address)
        }
    }

    fn authorize_resolution(&self, address: IpAddr) -> Result<(), DenyReason> {
        if address == self.0.ip() {
            Ok(())
        } else {
            PublicDestinations.authorize_resolution(address)
        }
    }
}

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
    let prepared =
        tokio::task::spawn_blocking(move || Prepared::build_debug(&build_project, &wasm)).await??;
    let session = project
        .module
        .parent()
        .context("the debug module must belong to a session directory")?;
    let app = LocalApp::start_debug(prepared, &session.join("runtime"), peer).await?;
    let result = serve(style, &project, app.clone(), port, json).await;
    app.proxy.shutdown_sockets();
    tokio::task::spawn_blocking(move || app.cell.shutdown()).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_debug_peer_bypasses_private_egress_rejection() {
        let endpoint: SocketAddr = "127.0.0.1:12345".parse().unwrap();
        let policy = DebugPeer::new(endpoint).unwrap();
        assert!(
            policy.authorize(endpoint).is_ok(),
            "the owned debugger peer must be reachable"
        );
        assert!(
            policy.authorize_resolution(endpoint.ip()).is_ok(),
            "the owned peer IP must resolve before its exact port is checked"
        );
        for rejected in [
            "127.0.0.1:12346",
            "127.0.0.2:12345",
            "10.0.0.1:12345",
            "[::1]:12345",
        ] {
            assert!(
                policy.authorize(rejected.parse().unwrap()).is_err(),
                "a debugger exception must not grant private egress to {rejected}"
            );
        }
        assert!(
            policy
                .authorize_resolution("10.0.0.1".parse().unwrap())
                .is_err(),
            "debugging must not broaden private DNS answers"
        );
        assert!(
            policy.authorize("8.8.8.8:443".parse().unwrap()).is_ok(),
            "ordinary public egress must remain available"
        );
    }

    #[test]
    fn debugger_peer_configuration_requires_a_bound_ipv4_loopback_endpoint() {
        for rejected in [
            "127.0.0.1:0",
            "0.0.0.0:12345",
            "8.8.8.8:12345",
            "[::1]:12345",
        ] {
            assert!(
                DebugPeer::new(rejected.parse().unwrap()).is_err(),
                "invalid debugger endpoint {rejected} must fail before listening"
            );
        }
    }
}
