//! Local application preparation and immutable compiler-owned artifacts.

use std::path::Path;

use wisp_abi::config::{AppManifest, InitSandbox};

/// Local compiler output store and preparation entry point.
pub struct Deployer {
    root: std::path::PathBuf,
}

impl Deployer {
    /// Opens the private store populated by this host's preparation compiler.
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        private_artifact_directory(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }
}

/// Host-selected preparation provider and its explicit resource policy.
pub struct PreparationContext<'a> {
    /// Engine configuration and decoder used by the consuming cells.
    pub engine: &'a wisp_engine::WispEngine,
    /// Controlled local preparation compiler selected by host composition.
    pub provider: &'a dyn wisp_prepare::Preparation,
    /// Runtime provenance established by the host, never inferred from guest metadata.
    pub runtime: wisp_prepare::RuntimeProfile,
    /// Initialization CPU backstop across all replay passes.
    pub cpu_ms: u32,
    /// Initialization wall backstop per call.
    pub wall_ms: u32,
    /// Maximum replay passes.
    pub max_passes: u32,
    /// Retained successful-pass guest log bytes.
    pub log_bytes: usize,
    /// Required publication integrity mode; failures never weaken it.
    pub storage: wisp_engine::prepared::PreparedStorage,
    /// Cancellation of this owned preparation job.
    pub cancellation: &'a tokio_util::sync::CancellationToken,
}

/// Locally compiled output ready for an existing engine to check and load.
pub struct PreparedDeployment {
    /// Newly published immutable artifact directory.
    directory: std::path::PathBuf,
    /// Exact source, runtime and initialization identity expected by execution.
    identity: wisp_engine::prepared::PreparedIdentity,
    /// Actual initialization observations from the local provider.
    pub initialized: wisp_prepare::Initialized,
    /// Provider-reported initialization duration.
    pub preinit_ms: f64,
    /// Provider-reported native preparation duration.
    pub compile_ms: f64,
    /// Measured artifact publication duration.
    pub publication_ms: f64,
}

impl Deployer {
    pub(crate) fn load_prepared(
        &self,
        engine: &wisp_engine::WispEngine,
        deployed: &PreparedDeployment,
    ) -> anyhow::Result<wisp_engine::Template> {
        Ok(unsafe {
            // SAFETY: this deployer publishes controlled compiler output under its checked private root; published payloads remain unchanged.
            engine.load_prepared(&deployed.directory, deployed.identity)
        }?)
    }

    /// Prepare and publish local compiler output with its actual producer observations.
    pub async fn deploy_prepared(
        &self,
        manifest: &AppManifest,
        wasm: &[u8],
        init_sandbox: InitSandbox,
        context: &PreparationContext<'_>,
    ) -> anyhow::Result<PreparedDeployment> {
        admit_local(manifest, wasm)?;
        let initialization = wisp_prepare::Initialization {
            config: serde_json::to_vec(&manifest.init_visible_vars)?,
            sandbox: init_sandbox,
            cpu_ms: context.cpu_ms,
            wall_ms: context.wall_ms,
            max_passes: context.max_passes,
            log_bytes: context.log_bytes,
        };
        let request = wisp_prepare::BuildRequest {
            engine: context.engine.configuration(),
            runtime: context.runtime.clone(),
            plan: manifest.limits,
            initialization,
            input_hash: *blake3::hash(wasm).as_bytes(),
        };
        let identity = request.identity()?;
        let product = context
            .provider
            .prepare(wasm, &request, context.cancellation)
            .await?;
        // SAFETY: only the host-selected trusted Preparation provider supplies these bytes;
        // source uploads never reach this decoder as native artifacts.
        let artifact = unsafe {
            context
                .engine
                .decode_builder_output(product.prepared_bytes(), identity)
        }?;
        let publication = tempfile::tempdir_in(&self.root)?;
        let directory = publication.path().join("prepared");
        let clock = std::time::Instant::now();
        artifact.publish(&directory, context.storage)?;
        let publication_ms = clock.elapsed().as_secs_f64() * 1000.0;
        let _published_directory = publication.keep();
        Ok(PreparedDeployment {
            directory,
            identity,
            initialized: product.initialized,
            preinit_ms: product.preinit_ms,
            compile_ms: product.compile_ms,
            publication_ms,
        })
    }
}

fn admit_local(
    manifest: &AppManifest,
    wasm: &[u8],
) -> anyhow::Result<wisp_control::validator::ValidationReport> {
    let report = wisp_control::validate(wasm, manifest.limits).map_err(|violations| {
        anyhow::anyhow!(
            "module rejected:\n{}",
            violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
    })?;
    let compressed_bytes = zstd::encode_all(wasm, 19)?.len() as u64;
    let limit_bytes = manifest.limits.module_bytes_compressed;
    anyhow::ensure!(compressed_bytes <= limit_bytes, "compressed module is {compressed_bytes} bytes, exceeding the configured {limit_bytes} byte limit");
    Ok(report)
}

/// Creates or checks the operator-owned directory that holds local compiler output.
fn private_artifact_directory(root: &std::path::Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        match std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = std::fs::symlink_metadata(root)?;
        anyhow::ensure!(
            metadata.is_dir()
                && metadata.uid() == rustix::process::geteuid().as_raw()
                && metadata.mode() & 0o7077 == 0,
            "prepared artifact directory must be owned by this user, accessible only to its owner and contain no links: {}",
            root.display()
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        anyhow::bail!("private prepared artifacts require Unix filesystem ownership and modes")
    }
}

impl PreparedDeployment {
    /// Immutable directory published by this preparation.
    pub fn directory(&self) -> &std::path::Path {
        &self.directory
    }
    /// Identity recorded by this preparation compiler.
    pub fn identity(&self) -> wisp_engine::prepared::PreparedIdentity {
        self.identity
    }
}
