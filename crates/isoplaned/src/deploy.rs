//! Locally compiled preparation artifacts and immutable application snapshots.

use std::path::Path;

use isoplane_abi::config::AppManifest;
use isoplane_control::DeployOptions;

#[cfg(target_os = "linux")]
mod baseline;
mod settings;

/// The file in a layered version's artifact directory naming its baseline.
const BASELINE_FILE: &str = "baseline";

/// Prepares immutable local artifacts without retaining deployment history.
pub struct Deployer {
    root: std::path::PathBuf,
    #[cfg(target_os = "linux")]
    baselines: baseline::Baselines,
}

impl Deployer {
    /// Opens the private store populated by this host's preparation compiler.
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        isoplane_control::private_files::directory(root)?;
        Ok(Deployer {
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

/// A publication protected from collection until selected or discarded.
pub struct PreparedInstallation {
    /// The exact immutable output to select.
    pub receipt: PreparedReceipt,
    pub(crate) lease: isoplane_prepare::PreparationOwner,
    /// Actual initialization observations.
    pub initialized: isoplane_prepare::Initialized,
    /// Measured producer stages.
    pub stages: Vec<crate::node::DeployStage>,
}

impl PreparedInstallation {
    /// Transfers artifact ownership and returns the producer observations separately.
    pub fn into_application(
        self,
    ) -> (
        crate::installed::InstalledApplication,
        isoplane_prepare::Initialized,
        Vec<crate::node::DeployStage>,
    ) {
        (
            crate::installed::InstalledApplication::new(self.receipt, self.lease),
            self.initialized,
            self.stages,
        )
    }
}

/// A published artifact directory, still owned until its caller keeps it.
struct Published {
    lock: isoplane_prepare::PreparationOwner,
    publication: tempfile::TempDir,
    receipt: PreparedReceipt,
    publication_ms: f64,
}

/// An opaque directory name beneath this node's prepared-artifact registry.
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

/// Immutable local preparation output selected for an application.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedReceipt {
    /// Immutable deployment version.
    pub version: String,
    /// Node-local artifact identity; never an arbitrary filesystem path.
    pub artifact_key: ArtifactKey,
    /// SHA256 of the complete prepared manifest.
    pub manifest_sha256: String,
    /// Content digest of the owner-bound private settings snapshot alongside the artifact.
    pub settings_digest: String,
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
        // SAFETY: this operator-owned root contains only controlled local compiler output
        // published by this deployer. Publication never modifies payloads, and the engine
        // contains a later storage fault.
        Ok(unsafe {
            engine.restore_prepared(&artifact.join("prepared"), digest, runtime_profile, &[])
        }?)
    }
    /// Publish an application without retaining deployment history.
    /// Initializes the installed platform runtime as a shared baseline and
    /// the application as a layer over it on Linux.
    pub async fn prepare_application(
        &self,
        manifest: &AppManifest,
        wasm: &[u8],
        options: &DeployOptions,
        context: &PreparationContext<'_>,
    ) -> anyhow::Result<PreparedInstallation> {
        let (report, request, identity) =
            self.admit_request(manifest, wasm, options, context).await?;
        let (product, artifact, baseline) = match &context.runtime {
            #[cfg(target_os = "linux")]
            isoplane_prepare::RuntimeProfile::SharedRuntime { .. } => {
                anyhow::ensure!(
                    report.has_initialize()
                        && report
                            .optional_exports
                            .contains(&isoplane_abi::imports::exports::INITIALIZE_RUNTIME),
                    "installed runtime needs runtime and application initialization exports"
                );
                let held = self.baseline(wasm, &request, context).await?;
                let product = context
                    .provider
                    .prepare_over(&held.published, &request, context.cancellation)
                    .await?;
                // SAFETY: only the host-selected provider supplies native output; the baseline remains owned.
                let artifact = unsafe {
                    context.engine.decode_builder_output_over(
                        &held.template,
                        product.prepared_bytes(),
                        identity,
                    )
                }?;
                (product, artifact, Some(held.name))
            }
            isoplane_prepare::RuntimeProfile::RawWasm => {
                let product = context
                    .provider
                    .prepare(wasm, &request, context.cancellation)
                    .await?;
                // SAFETY: native output comes only from the selected trusted producer, never a bundle.
                let artifact = unsafe {
                    context
                        .engine
                        .decode_builder_output(product.prepared_bytes(), identity)
                }?;
                (product, artifact, None)
            }
            #[cfg(not(target_os = "linux"))]
            isoplane_prepare::RuntimeProfile::SharedRuntime { .. } => {
                anyhow::ensure!(
                    report.has_initialize(),
                    "installed runtime needs application initialization"
                );
                let product = context
                    .provider
                    .prepare(wasm, &request, context.cancellation)
                    .await?;
                // SAFETY: as for RawWasm above; the profile changes admission, not producer trust.
                let artifact = unsafe {
                    context
                        .engine
                        .decode_builder_output(product.prepared_bytes(), identity)
                }?;
                (product, artifact, None)
            }
        };
        let published = self
            .publish(&artifact, manifest, options, context, baseline.as_deref())
            .await?;
        let stages = vec![
            crate::node::DeployStage {
                name: "preinit".into(),
                ms: product.preinit_ms,
            },
            crate::node::DeployStage {
                name: "prepared_encoding".into(),
                ms: product.compile_ms,
            },
            crate::node::DeployStage {
                name: "publish".into(),
                ms: published.publication_ms,
            },
        ];
        let _path = published.publication.keep();
        Ok(PreparedInstallation {
            receipt: published.receipt,
            lease: published.lock,
            initialized: product.initialized,
            stages,
        })
    }

    /// Admits `wasm` and builds the request that prepares it.
    async fn admit_request<'a>(
        &self,
        manifest: &'a AppManifest,
        wasm: &'a [u8],
        opts: &DeployOptions,
        context: &PreparationContext<'_>,
    ) -> anyhow::Result<(
        isoplane_runtime::validator::ValidationReport,
        isoplane_prepare::BuildRequest,
        isoplane_engine::prepared::PreparedIdentity,
    )> {
        anyhow::ensure!(
            !opts.tenant.trim().is_empty() && context.network.tenant == opts.tenant,
            "preparation requires the established application owner"
        );
        anyhow::ensure!(
            matches!(
                context.network.destinations,
                isoplane_prepare::InitializationDestinations::Public { .. }
            ),
            "daemon preparation requires explicit public destination policy"
        );
        let admitted = match context.runtime {
            isoplane_prepare::RuntimeProfile::SharedRuntime { .. } => {
                isoplane_control::admission::admit_shared_runtime(manifest, wasm)?
            }
            isoplane_prepare::RuntimeProfile::RawWasm => {
                isoplane_control::admission::admit(manifest, wasm)?
            }
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
            memory_limit: manifest.memory_limit(),
            initialization,
            input_hash: *blake3::hash(wasm).as_bytes(),
        };
        let identity = request.identity()?;
        Ok((admitted, request, identity))
    }

    /// Publishes `artifact` in a new artifact directory, naming the baseline
    /// it was prepared over, if any, and returns its selection receipt.
    async fn publish(
        &self,
        artifact: &isoplane_engine::prepared::PreparedTemplate,
        manifest: &AppManifest,
        opts: &DeployOptions,
        context: &PreparationContext<'_>,
        baseline: Option<&str>,
    ) -> anyhow::Result<Published> {
        let (publication, publication_lock) = crate::installed::new_publication(&self.root)?;
        if let Some(name) = baseline {
            isoplane_control::private_files::write_new(
                &publication.path().join(BASELINE_FILE),
                name.as_bytes(),
            )?;
        }
        let directory = publication.path().join("prepared");
        let clock = std::time::Instant::now();
        artifact.publish(&directory)?;
        let mut receipt = PreparedReceipt {
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
            settings_digest: String::new(),
            runtime_profile: context.runtime.clone(),
            manifest: manifest.clone(),
        };
        receipt.settings_digest = self
            .write_runtime_settings(
                publication.path(),
                &opts.tenant,
                &receipt,
                &context.runtime_configuration,
                match &context.network.destinations {
                    isoplane_prepare::InitializationDestinations::Public { exceptions } => {
                        exceptions.clone()
                    }
                    isoplane_prepare::InitializationDestinations::LocalDevelopment => {
                        anyhow::bail!(
                            "daemon preparation requires explicit public destination policy"
                        )
                    }
                },
            )
            .await?;
        std::fs::File::open(publication.path())?.sync_all()?;
        std::fs::File::open(&self.root)?.sync_all()?;
        let publication_ms = clock.elapsed().as_secs_f64() * 1000.0;
        Ok(Published {
            publication,
            lock: publication_lock,
            receipt,
            publication_ms,
        })
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::ArtifactKey;

    #[test]
    fn artifact_keys_cannot_escape_the_node_owned_registry() {
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
                "catalog artifact key must not name arbitrary paths: {key}"
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
