//! Foreground local hosting and offline application deployment.

use anyhow::Context;
use clap::Parser;
use isoplaned::cli::{Args, Command};
use isoplaned::config::NodeConfig;
use isoplaned::node::Node;

/// The process's open-file limits.
#[cfg(unix)]
#[derive(Clone, Copy)]
struct OpenFileLimit {
    soft: libc::rlim_t,
    hard: libc::rlim_t,
}

#[cfg(unix)]
fn open_file_limit() -> std::io::Result<OpenFileLimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` with a valid resource and an owned, fully
    // initialized `rlimit`; it does not retain the pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(OpenFileLimit {
        soft: limit.rlim_cur,
        hard: limit.rlim_max,
    })
}

/// Raise the open-file soft limit to the hard limit, which needs no
/// privilege. Every mapped template holds its `.cwasm` open and every
/// outbound connection holds a socket, so the default soft limit of 1024 is
/// the first ceiling a dense node meets. The hard limit is the
/// administrator's and is not touched.
#[cfg(unix)]
fn raise_open_file_limit(current: OpenFileLimit) -> std::io::Result<OpenFileLimit> {
    let limit = libc::rlimit {
        rlim_cur: current.hard,
        rlim_max: current.hard,
    };
    // SAFETY: `setrlimit` with a valid resource and an owned, fully
    // initialized `rlimit`; it does not retain the pointer.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(OpenFileLimit {
        soft: current.hard,
        hard: current.hard,
    })
}

#[cfg(unix)]
fn raise_and_log_open_file_limit() {
    match open_file_limit() {
        Err(error) => {
            tracing::warn!(%error, "could not read RLIMIT_NOFILE; the open-file limit is unknown")
        }
        Ok(limit) if limit.soft >= limit.hard => tracing::info!(
            soft = limit.soft,
            hard = limit.hard,
            "RLIMIT_NOFILE soft limit is already its hard limit"
        ),
        Ok(limit) => match raise_open_file_limit(limit) {
            Ok(raised) => tracing::info!(
                from = limit.soft,
                soft = raised.soft,
                hard = raised.hard,
                "raised RLIMIT_NOFILE soft limit to its hard limit"
            ),
            Err(error) => tracing::warn!(
                %error,
                soft = limit.soft,
                hard = limit.hard,
                "could not raise RLIMIT_NOFILE soft limit; keeping it"
            ),
        },
    }
}

fn main() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(isoplane_host::threads::ThreadAllocation::production().outer)
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Deploy(args) => {
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .try_init()
                .map_err(|error| anyhow::anyhow!("initialize deployment logging: {error}"))?;
            let cfg = NodeConfig::load(&args.config)?;
            let cancellation = tokio_util::sync::CancellationToken::new();
            let cancelled = cancellation.clone();
            let signal = tokio::spawn(async move {
                if let Err(error) = termination().await {
                    eprintln!("reading shutdown signal: {error}");
                }
                cancelled.cancel();
            });
            let result =
                isoplaned::node::deploy::deploy(cfg, &args.app, &args.bundle, &cancellation).await;
            signal.abort();
            let deployed = result?;
            println!("{}", serde_json::to_string(&deployed)?);
            Ok(())
        }
        Command::Serve(args) => {
            let mut cfg = NodeConfig::load(&args.config)?;
            if let Some(endpoint) = args.otlp_logs_endpoint {
                cfg.otlp_logs_endpoint = Some(endpoint);
            }
            if let Some(endpoint) = args.otlp_traces_endpoint {
                cfg.otlp_traces_endpoint = Some(endpoint);
            }
            cfg.validate()?;
            if args.print_config {
                println!("{}", toml::to_string_pretty(&cfg)?);
                return Ok(());
            }
            if args.check {
                println!("ok: static host configuration");
                return Ok(());
            }
            let logging_cfg = cfg.clone();
            let logging = tokio::task::spawn_blocking(move || {
                isoplaned::logs::init_with_limits(
                    !args.plain_logs,
                    std::io::stderr,
                    &logging_cfg.node,
                    logging_cfg.otlp_logs_endpoint.as_ref(),
                    logging_cfg.otlp_traces_endpoint.as_ref(),
                    logging_cfg.trace_limits,
                )
            })
            .await??;
            let result = run_node(cfg, args.shutdown_on_stdin_close).await;
            let export = tokio::task::spawn_blocking(move || logging.shutdown()).await?;
            if let Err(error) = export {
                eprintln!("telemetry shutdown failed: {error}");
                if result.is_ok() {
                    return Err(error.into());
                }
            }
            result
        }
    }
}

async fn termination() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { result = tokio::signal::ctrl_c() => result, _ = terminate.recv() => Ok(()) }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

async fn stop(stdin: bool) -> std::io::Result<()> {
    if !stdin {
        return termination().await;
    }
    let (closed, receive) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let result = std::io::copy(
            &mut std::io::stdin().lock().take(u64::MAX),
            &mut std::io::sink(),
        )
        .map(|_| ());
        // A signal may have already shut down the receiver.
        drop(closed.send(result));
    });
    tokio::select! {
        result = termination() => result,
        result = receive => result.map_err(std::io::Error::other)?,
    }
}

async fn run_node(cfg: NodeConfig, stdin: bool) -> anyhow::Result<()> {
    #[cfg(unix)]
    raise_and_log_open_file_limit();
    let proxy = tokio::net::TcpListener::bind(cfg.proxy.listen)
        .await
        .with_context(|| format!("binding application listener {}", cfg.proxy.listen))?;
    let diagnostics = tokio::net::TcpListener::bind(cfg.admin.listen)
        .await
        .with_context(|| format!("binding diagnostic listener {}", cfg.admin.listen))?;
    let node = tokio::task::spawn_blocking(move || Node::build(cfg)).await??;
    let result = async {
        node.restore_installed().await?;
        tracing::info!(node = %node.cfg.node, proxy = %node.cfg.proxy.listen, apps = node.cfg.apps.len(), "local host serving");
        serve_node(node.clone(), proxy, diagnostics, stop(stdin)).await
    }.await;
    node.shutdown();
    node.wait_for_shutdown().await?;
    tokio::task::spawn_blocking(move || drop(node)).await?;
    result
}

async fn serve_node(
    node: std::sync::Arc<Node>,
    proxy_listener: tokio::net::TcpListener,
    admin_listener: tokio::net::TcpListener,
    stop: impl std::future::Future<Output = std::io::Result<()>>,
) -> anyhow::Result<()> {
    let admin_stop = tokio_util::sync::CancellationToken::new();
    let admin_connections = tokio_util::task::TaskTracker::new();
    let mut listeners = tokio::task::JoinSet::new();
    listeners.spawn(node.clone().serve(proxy_listener));
    listeners.spawn(isoplaned::http::serve_until(
        node.clone(),
        admin_listener,
        admin_stop.clone(),
        admin_connections.clone(),
    ));

    let result = tokio::select! {
        result = listeners.join_next() => match result {
            Some(Ok(result)) => result,
            Some(Err(error)) => Err(error.into()),
            None => Ok(()),
        },
        result = stop => result.map_err(Into::into),
    };
    tracing::info!("stopping listeners and cancelling transports");
    node.shutdown();
    admin_stop.cancel();
    let mut result = result;
    while let Some(listener) = listeners.join_next().await {
        let stopped = match listener {
            Ok(stopped) => stopped,
            Err(error) => Err(error.into()),
        };
        if let Err(error) = stopped {
            tracing::error!(error = %error, "listener failed during shutdown");
            if result.is_ok() {
                result = Err(error);
            }
        }
    }
    node.wait_for_shutdown().await?;
    admin_connections.close();
    admin_connections.wait().await;
    tracing::info!("listeners and transports stopped");
    result
}
