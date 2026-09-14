//! Build, serve and reload a local project using the shared guest runtime.

mod build;
pub mod debug;
mod destinations;
mod logs;
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

pub(crate) fn init_logging(json: bool, color: bool) -> anyhow::Result<()> {
    logs::init(json, color)
}

/// Reads activation-only settings from a bounded private parent pipe.
pub fn read_runtime_configuration(
    from_input: bool,
) -> anyhow::Result<wisp_host::bindings::RuntimeConfiguration> {
    if !from_input {
        return Ok(wisp_host::bindings::RuntimeConfiguration::from_json(b"{}")?);
    }
    read_configuration_line(std::io::stdin().lock())
}

fn read_configuration_line(
    input: impl std::io::BufRead,
) -> anyhow::Result<wisp_host::bindings::RuntimeConfiguration> {
    use std::io::BufRead;
    let mut bytes = Vec::new();
    input
        .take(wisp_abi::imports::MAX_OUT_BUFFER_BYTES as u64 + 2)
        .read_until(b'\n', &mut bytes)?;
    anyhow::ensure!(
        bytes.last() == Some(&b'\n'),
        "runtime configuration pipe must supply one complete JSON line"
    );
    bytes.pop();
    Ok(wisp_host::bindings::RuntimeConfiguration::from_json(
        &bytes,
    )?)
}

/// Resolves the configured module without building it or requiring it to exist.
/// Fails when the project directory or local manifest is invalid.
pub fn project_module(directory: &Path) -> anyhow::Result<PathBuf> {
    Ok(Project::load(directory, None, None, true)?.module)
}

pub struct DevOpts {
    pub runtime_configuration: wisp_host::bindings::RuntimeConfiguration,
    pub json: bool,
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
    let mut lock = project_lock(&project.root)?;
    let _owner = lock.try_write().context(
        "opening local development state; another wisp dev may already own this project",
    )?;
    let mut observed = project.fingerprint(opts.watch_module_only)?;
    let prepared = {
        let cancellation = CancellationToken::new();
        let preparation = prepare(
            &project,
            &cancellation,
            opts.runtime_configuration.clone(),
            opts.json,
        );
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
    let mut app = LocalApp::start(
        prepared,
        &data,
        opts.runtime_configuration.clone(),
    )
    .await?;
    let result = serve(st, &opts, project, observed, &mut app).await;
    app.proxy.shutdown_sockets();
    tokio::task::spawn_blocking(move || app.cell.shutdown()).await?;
    result
}

fn project_lock(root: &Path) -> anyhow::Result<fd_lock::RwLock<std::fs::File>> {
    let work = root.join(".wisp");
    std::fs::create_dir_all(&work)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(work.join("dev.lock"))?;
    Ok(fd_lock::RwLock::new(file))
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
    configuration: wisp_host::bindings::RuntimeConfiguration,
    json: bool,
) -> anyhow::Result<Option<Prepared>> {
    if let Some(command) = &project.build {
        eprintln!("  building {}", project.manifest.name);
        if !build::run(&project.root, command, cancellation).await? {
            return Ok(None);
        }
    }
    if !json {
        eprintln!("  preparing {} for local use...", project.manifest.name);
    }
    let bytes = std::fs::read(&project.module).with_context(|| {
        format!(
            "reading {}; set [dev].build or --build to produce this module",
            project.module.display()
        )
    })?;
    let project = project.clone();
    tokio::task::spawn_blocking(move || Prepared::build(&project, &bytes, &configuration))
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
    report(st, opts.json, "ready", &project.manifest.name, address)?;
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
                        let configuration = opts.runtime_configuration.clone();
                        let json = opts.json;
                        rebuild = Some(Box::pin(async move {
                            let prepared =
                                prepare(&next_project, &cancellation, configuration, json).await?;
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
                        *app = LocalApp::start(prepared, &next_project.state_dir(), opts.runtime_configuration.clone()).await?;
                        *current_proxy.write() = app.proxy.clone();
                        project = next_project;
                        observed.module = project.fingerprint(opts.watch_module_only)?.module;
                        report(st, opts.json, "reloaded", &project.manifest.name, address)?;
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

async fn stop_connections<T: 'static>(connections: &mut JoinSet<T>) {
    connections.abort_all();
    while let Some(joined) = connections.join_next().await {
        if let Err(e) = joined {
            if !e.is_cancelled() {
                tracing::warn!(error = %e, "local connection failed during shutdown");
            }
        }
    }
}

fn report(
    st: &Style,
    json: bool,
    event: &str,
    app: &str,
    address: std::net::SocketAddr,
) -> anyhow::Result<()> {
    if json {
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({ "event": event, "app": app, "url": format!("http://{address}") }),
        )?;
        output.write_all(b"\n")?;
        output.flush()?;
    } else {
        println!(
            "  {} {app} at http://{address} (local guest execution)",
            st.green(event)
        );
    }
    Ok(())
}

#[cfg(test)]
mod configuration_tests {
    use super::read_configuration_line;

    #[test]
    fn activation_configuration_requires_a_complete_parent_frame() {
        assert!(
            read_configuration_line(&b"{}"[..]).is_err(),
            "a truncated parent frame must not start an application"
        );
    }

    #[test]
    fn activation_configuration_rejects_oversized_parent_input() {
        let bytes = vec![b' '; wisp_abi::imports::MAX_OUT_BUFFER_BYTES + 3];
        assert!(
            read_configuration_line(bytes.as_slice()).is_err(),
            "an unbounded parent frame must be refused before reading further"
        );
    }

    #[test]
    fn activation_configuration_leaves_following_parent_input_unconsumed() {
        let mut bytes = &b"{}\nnext"[..];
        assert!(
            read_configuration_line(&mut bytes).is_ok(),
            "a complete settings map must be accepted"
        );
        assert_eq!(
            bytes, b"next",
            "the settings reader must consume only its own frame"
        );
    }
}
