//! Bounded local initialization and installed runtime selection.

use serde::{Deserialize, Serialize};

/// Trusted local preparation policy; initialization still runs inside the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationConfig {
    /// Initialization occupancy budget across replay attempts.
    #[serde(default = "default_occupancy_ms")]
    pub initialization_occupancy_ms: u32,
    /// Total initialization wall deadline, including waits and replay passes.
    #[serde(default = "default_wall_ms")]
    pub initialization_wall_ms: u32,
    /// Maximum impurity-deferral replay attempts.
    #[serde(default = "default_passes")]
    pub initialization_passes: u32,
    /// Maximum retained successful initialization log bytes.
    #[serde(default = "default_log_bytes")]
    pub initialization_log_bytes: usize,
    /// The shared runtimes installed with this node, one directory per SDK
    /// release beside the `inventory.json` listing them. Application packages
    /// are prepared and restored only with this inventory. Required when the
    /// host configures any applications; static validation does not read its files.
    #[serde(default)]
    pub runtimes: Option<std::path::PathBuf>,
}

impl PreparationConfig {
    /// Refuse invalid static limits and unsupported profile declarations before starting services.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.initialization_occupancy_ms > 0
                && self.initialization_wall_ms > 0
                && self.initialization_passes > 0
                && self.initialization_log_bytes > 0,
            "preparation initialization limits must be positive"
        );
        Ok(())
    }
}

/// Prepares and verifies one portable application without acquiring any serving resources.
pub(crate) async fn prepare(
    cfg: &crate::config::NodeConfig,
    manifest: &isoplane_abi::config::AppManifest,
    bytes: &[u8],
    options: isoplane_control::DeployOptions,
    cancellation: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<crate::deploy::PreparedInstallation> {
    use crate::settings::RuntimeSettingsSource;
    let policy = cfg.preparation.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "local deployment requires [preparation] runtime and initialization settings"
        )
    })?;
    let runtime_directory = policy.runtimes.as_ref().ok_or_else(|| {
        anyhow::anyhow!("preparation.runtimes must name the installed SDK runtime directory")
    })?;
    let runtime = crate::runtimes::Runtimes::load(runtime_directory)?;
    let package = runtime.resolve(
        bytes,
        isoplane_prepare::package::PackageLimits::of(manifest),
    )?;
    let source = crate::settings::LocalFiles::configured(
        cfg.owner.clone(),
        cfg.application_settings.clone(),
    )?;
    let settings = source.settings(crate::settings::SettingsOwner {
        tenant: &cfg.owner,
        application: &manifest.name,
    })?;
    let trust = cfg.trust_bundle()?;
    let engine = isoplane_engine::IsoplaneEngine::new(crate::real::engine_config(cfg, &trust)?)?;
    let paths = crate::real::RealPaths::under(&cfg.data_dir);
    isoplane_engine::CompilationCache::create_directory(&paths.artifacts)?;
    // SAFETY: this protected host-owned cache is populated only by atomic compiler publication.
    let cache =
        unsafe { isoplane_engine::CompilationCache::from_trusted_directory(paths.artifacts) }?;
    let provider = isoplane_prepare::LocalPreparation::with_cache(std::sync::Arc::new(cache))?;
    let deployer = crate::deploy::Deployer::open(&paths.registry)?;
    let context = crate::deploy::PreparationContext {
        network: crate::real::initialization_network(
            &cfg.owner,
            manifest,
            settings.allowed_destinations,
            cfg.network,
            &cfg.host,
        ),
        runtime_configuration: settings.configuration,
        engine: &engine,
        provider: &provider,
        runtime: package.profile,
        application_files: package.files,
        occupancy_ms: policy.initialization_occupancy_ms,
        wall_ms: policy.initialization_wall_ms,
        max_passes: policy.initialization_passes,
        log_bytes: policy.initialization_log_bytes,
        cancellation,
    };
    let prepared = deployer
        .prepare_application(manifest, &package.runtime, &options, &context)
        .await?;
    deployer
        .runtime_settings(&cfg.owner, &prepared.receipt)
        .await?;
    let _template = deployer.restore_prepared(&engine, &prepared.receipt)?;
    Ok(prepared)
}

fn default_occupancy_ms() -> u32 {
    isoplane_prepare::preinit::DEFAULT_INITIALIZATION_OCCUPANCY_MS
}
fn default_wall_ms() -> u32 {
    isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS
}
fn default_passes() -> u32 {
    isoplane_prepare::preinit::DEFAULT_INITIALIZATION_PASSES
}
fn default_log_bytes() -> usize {
    isoplane_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES
}
