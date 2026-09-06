//! Build, serve and reload a local project using the shared guest runtime.

mod project;
mod runtime;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use wisp_abi::config::InitSandbox;
use wisp_cli::render::Style;

use project::Project;
use runtime::{LocalApp, Prepared};

pub struct DevOpts {
    pub port: u16,
    pub watch: PathBuf,
    pub no_watch: bool,
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
    let mut observed = project.fingerprint()?;
    let prepared = tokio::select! {
        prepared = prepare(&project) => prepared?,
        stop = tokio::signal::ctrl_c() => { stop?; return Ok(()); }
    };
    observed.module = project.fingerprint()?.module;
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

async fn prepare(project: &Project) -> anyhow::Result<Prepared> {
    if let Some(command) = &project.build {
        println!("  building {}", project.manifest.name);
        let log_dir = project.root.join(".wisp");
        std::fs::create_dir_all(&log_dir)?;
        let log_path = log_dir.join("build.log");
        let log = std::fs::File::create(&log_path)?;
        let mut child = tokio::process::Command::new("bash")
            .args(["-c", command])
            .current_dir(&project.root)
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .context("starting the project's build command")?;
        let status = child.wait().await?;
        anyhow::ensure!(
            status.success(),
            "build failed ({status}); {}:\n{}",
            log_path.display(),
            std::fs::read_to_string(&log_path)?
        );
    }
    let bytes = std::fs::read(&project.module).with_context(|| {
        format!(
            "reading {}; set [dev].build or --build to produce this module",
            project.module.display()
        )
    })?;
    let project = project.clone();
    tokio::task::spawn_blocking(move || Prepared::build(&project, &bytes)).await?
}

type Rebuild =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<(Project, Prepared)>>>>;

async fn next_build(rebuild: &mut Option<Rebuild>) -> anyhow::Result<(Project, Prepared)> {
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
            result = &mut stop => { result?; break; }
            connection = listener.accept() => {
                let (stream, peer) = connection?;
                let current_proxy = current_proxy.clone();
                connections.spawn(serve_connection(stream, peer, current_proxy));
            }
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(e)) = joined { tracing::warn!(error = %e, "local connection task failed"); }
            }
            _ = interval.tick(), if !opts.no_watch && rebuild.is_none() => {
                let next = project.fingerprint()?;
                if next == observed { continue; }
                observed = next;
                match load(opts) {
                    Ok(next_project) => {
                        rebuild = Some(Box::pin(async move {
                            let prepared = prepare(&next_project).await?;
                            Ok((next_project, prepared))
                        }));
                    }
                    Err(e) => eprintln!("wisp: reload failed; keeping the last working app: {e:#}"),
                }
            }
            result = next_build(&mut rebuild) => {
                rebuild = None;
                match result {
                    Ok((next_project, prepared)) => {
                        app.proxy.shutdown_sockets();
                        let old = app.clone();
                        tokio::task::spawn_blocking(move || old.cell.shutdown()).await?;
                        *app = LocalApp::start(prepared, &next_project.state_dir()).await?;
                        *current_proxy.write() = app.proxy.clone();
                        project = next_project;
                        observed.module = project.fingerprint()?.module;
                        println!("  {} {} at http://{address}", st.green("reloaded"), project.manifest.name);
                    }
                    Err(e) => {
                        eprintln!("wisp: reload failed; keeping the last working app: {e:#}");
                        observed.module = project.fingerprint()?.module;
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
