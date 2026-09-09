//! Build, serve and reload a local project using the shared guest runtime.

mod build;
mod project;
mod runtime;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use wisp_abi::config::InitSandbox;
use wisp_cli::render::Style;

use project::Project;
use runtime::{LocalApp, Prepared};

/// Resolves the configured module without building it or requiring it to exist.
/// Fails when the project directory or local manifest is invalid.
pub fn project_module(directory: &Path) -> anyhow::Result<PathBuf> {
    Ok(Project::load(directory, None, None, true)?.module)
}

pub struct DevOpts {
    pub port: u16,
    pub watch: PathBuf,
    pub no_watch: bool,
    /// Observe published modules and manifest changes while an external tool owns builds.
    pub watch_module_only: bool,
    pub build: Option<String>,
    pub no_build: bool,
    pub init_sandbox: Option<InitSandbox>,
    pub poll_ms: u64,
}

pub async fn dev(st: &Style, opts: DevOpts) -> anyhow::Result<()> {
    anyhow::ensure!(opts.poll_ms > 0, "--poll-ms must be greater than zero");
    let project = load(&opts)?;
    let work = project.root.join(".wisp");
    std::fs::create_dir_all(&work)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(work.join("dev.lock"))?;
    let mut lock = fd_lock::RwLock::new(file);
    let _owner = lock.try_write().context(
        "opening local development state; another wisp dev may already own this project",
    )?;
    let mut observed = project.fingerprint(opts.watch_module_only)?;
    let prepared = {
        let cancellation = CancellationToken::new();
        let preparation = prepare(&project, &cancellation);
        tokio::pin!(preparation);
        tokio::select! {
            prepared = &mut preparation => prepared?,
            stop = tokio::signal::ctrl_c() => {
                stop?;
                cancellation.cancel();
                preparation.await?;
                return Ok(());
            }
        }
    };
    let Some(prepared) = prepared else {
        return Ok(());
    };
    observed.module = project.fingerprint(opts.watch_module_only)?.module;
    let data = project.state_dir();
    let mut app = LocalApp::start(prepared, &data, ).await?;
    let result = serve(st, &opts, project, observed, &mut app).await;
    app.proxy.shutdown_sockets();
    tokio::task::spawn_blocking(move || app.cell.shutdown()).await?;
    result
}

fn load(opts: &DevOpts) -> anyhow::Result<Project> {
    Project::load(
        &opts.watch,
        opts.build.as_deref(),
        opts.init_sandbox,
        opts.no_build,
    )
}

async fn prepare(
    project: &Project,
    cancellation: &CancellationToken,
) -> anyhow::Result<Option<Prepared>> {
    if let Some(command) = &project.build {
        println!("  building {}", project.manifest.name);
        if !build::run(&project.root, command, cancellation).await? {
            return Ok(None);
        }
    }
    let bytes = std::fs::read(&project.module).with_context(|| {
        format!(
            "reading {}; set [dev].build or --build to produce this module",
            project.module.display()
        )
    })?;
    let project = project.clone();
    tokio::task::spawn_blocking(move || Prepared::build(&project, &bytes))
        .await?
        .map(Some)
}

type Rebuild = std::pin::Pin<
    Box<dyn std::future::Future<Output = anyhow::Result<Option<(Project, Prepared)>>>>,
>;

async fn next_build(rebuild: &mut Option<Rebuild>) -> anyhow::Result<Option<(Project, Prepared)>> {
    match rebuild {
        Some(build) => build.await,
        None => std::future::pending().await,
    }
}

async fn serve(
    st: &Style,
    opts: &DevOpts,
    mut project: Project,
    mut observed: project::Fingerprint,
    app: &mut Arc<LocalApp>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, opts.port)).await?;
    let address = listener.local_addr()?;
    let mut connections = JoinSet::new();
    let current_proxy = Arc::new(parking_lot::RwLock::new(app.proxy.clone()));
    let mut rebuild: Option<Rebuild> = None;
    let cancellation = CancellationToken::new();
    let mut interval = tokio::time::interval(Duration::from_millis(opts.poll_ms));
    println!(
        "  {} {} at http://{address} (local guest execution)",
        st.green("ready"),
        project.manifest.name
    );
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            result = &mut stop => {
                result?;
                cancellation.cancel();
                if let Some(build) = rebuild.take() {
                    build.await?;
                }
                break;
            }
            connection = listener.accept() => {
                let (stream, peer) = connection?;
                let current_proxy = current_proxy.clone();
                connections.spawn(serve_connection(stream, peer, current_proxy));
            }
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(e)) = joined { tracing::warn!(error = %e, "local connection task failed"); }
            }
            _ = interval.tick(), if !opts.no_watch && rebuild.is_none() => {
                let next = project.fingerprint(opts.watch_module_only)?;
                if next == observed { continue; }
                observed = next;
                match load(opts) {
                    Ok(next_project) => {
                        let cancellation = cancellation.clone();
                        rebuild = Some(Box::pin(async move {
                            let prepared = prepare(&next_project, &cancellation).await?;
                            Ok(prepared.map(|prepared| (next_project, prepared)))
                        }));
                    }
                    Err(e) => eprintln!("wisp: reload failed; keeping the last working app: {e:#}"),
                }
            }
            result = next_build(&mut rebuild) => {
                rebuild = None;
                match result {
                    Ok(Some((next_project, prepared))) => {
                        app.proxy.shutdown_sockets();
                        let old = app.clone();
                        tokio::task::spawn_blocking(move || old.cell.shutdown()).await?;
                        *app = LocalApp::start(prepared, &next_project.state_dir()).await?;
                        *current_proxy.write() = app.proxy.clone();
                        project = next_project;
                        observed.module = project.fingerprint(opts.watch_module_only)?.module;
                        println!("  {} {} at http://{address}", st.green("reloaded"), project.manifest.name);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("wisp: reload failed; keeping the last working app: {e:#}");
                        observed.module = project.fingerprint(opts.watch_module_only)?.module;
                    }
                }
            }
        }
    }
    stop_connections(&mut connections).await;
    Ok(())
}

async fn serve_connection<S>(
    stream: S,
    peer: std::net::SocketAddr,
    current_proxy: Arc<parking_lot::RwLock<Arc<wisp_proxy::Proxy>>>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = service_fn(move |request| current_proxy.read().clone().handle(request, peer));
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await
    {
        tracing::debug!(error = %e, "local connection closed");
    }
}

async fn stop_connections(connections: &mut JoinSet<()>) {
    connections.abort_all();
    while let Some(joined) = connections.join_next().await {
        if let Err(e) = joined {
            if !e.is_cancelled() {
                tracing::warn!(error = %e, "local connection failed during shutdown");
            }
        }
    }
}
