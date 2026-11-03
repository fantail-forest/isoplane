//! Local bundle preparation and atomic selection for the next daemon start.

use crate::config::NodeConfig;
use std::io::Read;
use std::path::Path;

/// The durable selection made by a completed local deployment.
#[derive(serde::Serialize)]
pub struct LocalDeployment {
    /// Selected application.
    pub app: String,
    /// Unique deployment identity; not a retained version-history entry.
    pub deployment: String,
    /// A running daemon retains its current applications until restarted.
    pub restart_required: bool,
    /// Actual initialization observations from the producer.
    pub initialization: isoplane_prepare::Initialized,
    /// Measured local preparation stages.
    pub stages: Vec<super::DeployStage>,
}

/// Prepares a portable bundle locally and selects it without contacting a serving daemon.
pub async fn deploy(
    cfg: NodeConfig,
    app: &str,
    bundle: &Path,
    cancellation: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<LocalDeployment> {
    cfg.validate()?;
    let manifest = cfg
        .apps
        .iter()
        .find(|candidate| candidate.manifest.name == app)
        .ok_or_else(|| anyhow::anyhow!("application `{app}` is not configured"))?
        .manifest
        .clone();
    let store = crate::installed::InstalledApplications::open(&cfg.data_dir)?;
    let _producer = store.deployment()?;
    let limits = isoplane_prepare::package::PackageLimits::of(&manifest);
    // Bound the input allocation separately from decoded payload bytes, allowing PAX headers,
    // long path metadata and block padding for every file written by the SDK bundle writer.
    let limit = limits
        .bytes
        .saturating_add(u64::from(limits.files).saturating_mul(8192))
        .saturating_add(10240);
    let mut bytes = Vec::new();
    std::fs::File::open(bundle)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= limit,
        "application `{app}` bundle exceeds {limit} bytes"
    );
    let owner = cfg.owner.clone();
    let version = hex::encode(rand::random::<[u8; 16]>());
    let result = async {
        let prepared = crate::preparation::prepare(
            &cfg,
            &manifest,
            &bytes,
            isoplane_control::DeployOptions {
                tenant: owner.clone(),
                version: isoplane_control::VersionId(version.clone()),
                init_sandbox: isoplane_abi::config::InitSandbox::RuntimeBringUp,
            },
            cancellation,
        )
        .await?;
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "application `{app}` deployment cancelled before selection"
        );
        store.select(&owner, &prepared)?;
        let initialization = prepared.initialized;
        let stages = prepared.stages;
        drop(prepared.lease);
        Ok(LocalDeployment {
            app: app.to_owned(),
            deployment: version,
            restart_required: true,
            initialization,
            stages,
        })
    }
    .await;
    if let Err(error) = store.collect() {
        tracing::warn!(%error, app, "retired artifact cleanup failed");
    }
    result
}
