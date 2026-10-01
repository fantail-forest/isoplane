//! Local application preparation and immutable compiler-owned artifacts.

use std::path::Path;

use isoplane_abi::config::AppManifest;

#[cfg(target_os = "linux")]
mod baseline;

/// The file in a layered version's artifact directory naming its baseline.
const BASELINE_FILE: &str = "baseline";

/// Local compiler output store and preparation entry point.
pub struct Deployer {
    root: std::path::PathBuf,
    #[cfg(target_os = "linux")]
    baselines: baseline::Baselines,
}

impl Deployer {
    /// Opens the private store populated by this host's preparation compiler.
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        private_artifact_directory(root)?;
        Ok(Self {
            root: root.to_path_buf(),
            #[cfg(target_os = "linux")]
            baselines: baseline::Baselines::default(),
        })
    }
}

/// Host-selected preparation provider and its explicit resource policy.
pub struct PreparationContext<'a> {
    /// Application outbound policy and host resource limits used during initialization.
    pub network: isoplane_prepare::InitializationNetwork,
    /// Owner-selected settings bound to this preparation.
    pub runtime_configuration: isoplane_host::bindings::RuntimeConfiguration,
    /// Engine configuration and decoder used by the consuming cells.
    pub engine: &'a isoplane_engine::IsoplaneEngine,
    /// Controlled local preparation compiler selected by host composition.
    pub provider: &'a dyn isoplane_prepare::Preparation,
    /// Runtime provenance established by the host, never inferred from guest metadata.
    pub runtime: isoplane_prepare::RuntimeProfile,
    /// Immutable application content served within this template only.
    pub application_files: isoplane_engine::PlatformFiles,
    /// Initialization occupancy budget across all replay passes.
    pub occupancy_ms: u32,
    /// Total initialization wall backstop, including waits and replay passes.
    pub wall_ms: u32,
    /// Maximum replay passes.
    pub max_passes: u32,
    /// Retained successful-pass guest log bytes.
    pub log_bytes: usize,
    /// Cancellation of this owned preparation job.
    pub cancellation: &'a tokio_util::sync::CancellationToken,
}

/// A published artifact directory, still owned until its caller keeps it.
struct Published {
    _publication_lock: isoplane_prepare::PreparationOwner,
    publication: tempfile::TempDir,
    receipt: PreparedReceipt,
}

/// An opaque directory name beneath this node's private artifact root.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ArtifactKey(String);

impl TryFrom<String> for ArtifactKey {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err("artifact key must contain only letters, digits, hyphens or underscores");
        }
        Ok(Self(value))
    }
}

impl From<ArtifactKey> for String {
    fn from(value: ArtifactKey) -> Self {
        value.0
    }
}

/// Immutable local preparation output selected by its content receipt.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedReceipt {
    /// Immutable deployment version.
    pub version: String,
    /// Node-local artifact identity; never an arbitrary filesystem path.
    pub artifact_key: ArtifactKey,
    /// SHA256 of the complete prepared manifest.
    pub manifest_sha256: String,
    /// Exact trusted producer provenance.
    pub runtime_profile: isoplane_prepare::RuntimeProfile,
    /// Runtime policy and application registration data.
    pub manifest: AppManifest,
}

impl Deployer {
    pub(crate) fn restore_prepared(
        &self,
        engine: &isoplane_engine::IsoplaneEngine,
        receipt: &PreparedReceipt,
    ) -> anyhow::Result<isoplane_engine::Template> {
        let digest: [u8; 32] = hex::decode(&receipt.manifest_sha256)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("prepared manifest SHA256 must be 32 bytes"))?;
        let artifact = self.root.join(&receipt.artifact_key.0);
        let runtime_profile = receipt.runtime_profile.digest()?;
        #[cfg(target_os = "linux")]
        if let Some(name) = baseline::named_by(&artifact)? {
            let base = self.base(engine, &name, runtime_profile)?;
            // SAFETY: as below; the base is a published baseline this deployer never modifies.
            return Ok(unsafe {
                engine.restore_prepared_over(
                    &base,
                    &artifact.join("prepared"),
                    digest,
                    runtime_profile,
                    &[],
                )
            }?);
        }
        // SAFETY: this checked operator-owned root holds controlled local compiler output; publication
        // creates new files and never modifies payloads. The engine contains a later storage fault.
        Ok(unsafe {
            engine.restore_prepared(&artifact.join("prepared"), digest, runtime_profile, &[])
        }?)
    }

    /// Prepare and publish a local immutable artifact.
    /// Initializes the installed platform runtime as a shared baseline and
    /// the application as a layer over it on Linux.
    pub async fn prepare_local(
        &self,
        manifest: &AppManifest,
        wasm: &[u8],
        options: &DeployOptions,
        context: &PreparationContext<'_>,
    ) -> anyhow::Result<PreparedReceipt> {
        let (admitted, request, identity) = self
            .admit_request(
                manifest,
                wasm,
                options,
                context,
                isoplane_prepare::PortableTemplate::Omitted,
            )
            .await?;
        anyhow::ensure!(
            admitted.has_initialize()
                && admitted
                    .optional_exports
                    .contains(&isoplane_abi::imports::exports::INITIALIZE_RUNTIME),
            "the installed runtime requires both runtime and application initialization exports"
        );
        #[cfg(target_os = "linux")]
        {
            let held = self.baseline(wasm, &request, context).await?;
            let product = context
                .provider
                .prepare_over(&held.published, &request, context.cancellation)
                .await?;
            // SAFETY: as for whole output below.
            let artifact = unsafe {
                context.engine.decode_builder_output_over(
                    &held.template,
                    product.prepared_bytes(),
                    identity,
                )
            }?;
            let published =
                self.publish(&artifact, manifest, options, context, Some(&held.name))?;
            let _directory = published.publication.keep();
            Ok(published.receipt)
        }
        // Only Linux maps a layer over a baseline, so elsewhere every module prepares whole.
        #[cfg(not(target_os = "linux"))]
        {
            drop(admitted);
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
            let published = self.publish(&artifact, manifest, options, context, None)?;
            let _directory = published.publication.keep();
            Ok(published.receipt)
        }
    }

    /// Admits `wasm` and builds the request that prepares it.
    async fn admit_request(
        &self,
        manifest: &AppManifest,
        wasm: &[u8],
        opts: &DeployOptions,
        context: &PreparationContext<'_>,
        portable_template: isoplane_prepare::PortableTemplate,
    ) -> anyhow::Result<(
        isoplane_control::validator::ValidationReport,
        isoplane_prepare::BuildRequest,
        isoplane_engine::prepared::PreparedIdentity,
    )> {
        let admitted = match context.runtime {
            isoplane_prepare::RuntimeProfile::SharedRuntime { .. } => {
                admit_local_shared_runtime(manifest, wasm)?
            }
            isoplane_prepare::RuntimeProfile::RawWasm => admit_local(manifest, wasm)?,
        };
        let mut runtime_config = Vec::new();
        context
            .runtime_configuration
            .write_json(&mut runtime_config)?;
        let initialization = isoplane_prepare::Initialization {
            network: Some(context.network.clone()),
            runtime_config,
            config: serde_json::to_vec(&manifest.init_visible_vars)?,
            sandbox: opts.init_sandbox,
            occupancy_ms: context.occupancy_ms,
            wall_ms: context.wall_ms,
            max_passes: context.max_passes,
            log_bytes: context.log_bytes,
        };
        let request = isoplane_prepare::BuildRequest {
            guest_memory_bytes: manifest.guest_memory_bytes,
            validation_limits: manifest.validation_limits,
            application_files: context.application_files.clone(),
            engine: context.engine.configuration(),
            runtime: context.runtime.clone(),
            plan: manifest.limits,
            initialization,
            input_hash: *blake3::hash(wasm).as_bytes(),
            portable_template,
        };
        let identity = request.identity()?;
        Ok((admitted, request, identity))
    }

    /// Publishes `artifact` in a new artifact directory, naming the baseline
    /// it was prepared over, if any, and returns its content receipt.
    fn publish(
        &self,
        artifact: &isoplane_engine::prepared::PreparedTemplate,
        manifest: &AppManifest,
        opts: &DeployOptions,
        context: &PreparationContext<'_>,
        baseline: Option<&str>,
    ) -> anyhow::Result<Published> {
        let publication = tempfile::Builder::new()
            .prefix("artifact-")
            .tempdir_in(&self.root)?;
        let publication_lock = isoplane_prepare::PreparationOwner::create(publication.path())?;
        std::fs::write(publication.path().join("version"), &opts.version.0)?;
        if let Some(name) = baseline {
            std::fs::write(publication.path().join(BASELINE_FILE), name)?;
        }
        let directory = publication.path().join("prepared");
        let clock = std::time::Instant::now();
        artifact.publish(&directory)?;
        let receipt = PreparedReceipt {
            version: opts.version.0.clone(),
            artifact_key: publication
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow::anyhow!("invalid generated artifact directory"))?
                .to_owned()
                .try_into()
                .map_err(anyhow::Error::msg)?,
            manifest_sha256: hex::encode(context.engine.prepared_manifest_sha256(&directory)?),
            runtime_profile: context.runtime.clone(),
            manifest: manifest.clone(),
        };
        let publication_ms = clock.elapsed().as_secs_f64() * 1000.0;
        tracing::info!(publication_ms, "local compiler artifact published");
        Ok(Published {
            publication,
            _publication_lock: publication_lock,
            receipt,
        })
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::ArtifactKey;

    #[test]
    fn artifact_keys_cannot_escape_the_private_artifact_root() {
        for key in [
            "",
            ".",
            "..",
            "../artifact",
            "/etc/passwd",
            "a/b",
            "a\\b",
            "a.b",
        ] {
            assert!(
                ArtifactKey::try_from(key.to_owned()).is_err(),
                "local artifact key must not name arbitrary paths: {key}"
            );
        }
        assert!(
            ArtifactKey::try_from("x".repeat(129)).is_err(),
            "artifact key length must be bounded"
        );
        assert!(
            ArtifactKey::try_from("artifact-abc_123".to_owned()).is_ok(),
            "generated opaque artifact names must be accepted"
        );
    }
}

/// The application version selected by the local preparation caller.
#[derive(Clone, Debug)]
pub struct VersionId(pub String);

/// Inputs used by local artifact publication and initialization.
#[derive(Clone, Debug)]
pub struct DeployOptions {
    /// Version named by the resulting local receipt.
    pub version: VersionId,
    /// Local initialization import policy.
    pub init_sandbox: isoplane_abi::config::InitSandbox,
}

fn admit_local(
    manifest: &AppManifest,
    wasm: &[u8],
) -> anyhow::Result<isoplane_control::validator::ValidationReport> {
    let report = isoplane_control::validator::validate_with_limits(
        wasm,
        manifest.limits,
        manifest.guest_memory_bytes,
        &manifest.validation_limits,
    )
    .map_err(|violations| {
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

fn admit_local_shared_runtime(
    manifest: &AppManifest,
    wasm: &[u8],
) -> anyhow::Result<isoplane_control::validator::ValidationReport> {
    isoplane_control::validator::validate_with_limits(
        wasm,
        manifest.limits,
        Some(isoplane_abi::config::GuestMemoryBytes::new(
            isoplane_abi::config::WASM32_MEMORY_MAX_BYTES,
        )?),
        &manifest.validation_limits,
    )
    .map_err(|violations| {
        anyhow::anyhow!(
            "shared runtime rejected:\n{}",
            violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
    })
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
