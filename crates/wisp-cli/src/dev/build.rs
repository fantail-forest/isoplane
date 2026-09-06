//! Build process ownership, diagnostics and cancellation for local development.

use std::path::Path;

use anyhow::Context;
use tokio_util::sync::CancellationToken;

/// Runs the build, returning false after cancellation has completed its cleanup.
/// Fails if the process cannot start, finish successfully, or be interrupted.
pub(super) async fn run(
    root: &Path,
    command: &str,
    cancellation: &CancellationToken,
) -> anyhow::Result<bool> {
    let log_dir = root.join(".wisp");
    std::fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join("build.log");
    let log = std::fs::File::create(&log_path)?;
    let mut child = tokio::process::Command::new("bash")
        .args(["-c", command])
        .current_dir(root)
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("starting the project's build command")?;
    let process_id = child.id().context("reading the build process ID")?;
    let status = tokio::select! {
        status = child.wait() => status?,
        _ = cancellation.cancelled() => {
            let signal = tokio::process::Command::new("kill")
                .args(["-INT", "--", &format!("-{process_id}")])
                .status().await.context("interrupting the project build")?;
            anyhow::ensure!(signal.success(), "could not interrupt project build {process_id}");
            child.wait().await.context("waiting for project build cleanup")?;
            return Ok(false);
        }
    };
    anyhow::ensure!(
        status.success(),
        "build failed ({status}); {}:\n{}",
        log_path.display(),
        std::fs::read_to_string(&log_path)?
    );
    Ok(true)
}
