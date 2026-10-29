//! Runtime baselines this node publishes once per runtime, engine and limits,
//! and prepares applications over as layers.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use isoplane_engine::{IsoplaneEngine, Template};
use isoplane_prepare::{BuildRequest, PublishedBaseline};

use super::{Deployer, PreparationContext, BASELINE_FILE};

const PREFIX: &str = "baseline-";
const LOCK: &str = "owner.lock";
const PREPARED: &str = "prepared";

/// Baselines loaded in the node's engine, shared by every version over one.
#[derive(Default)]
pub(super) struct Baselines(parking_lot::Mutex<HashMap<String, Template>>);

/// A baseline held throughout application preparation.
pub(super) struct HeldBaseline {
    pub name: String,
    pub published: PublishedBaseline,
    pub template: Template,
    _lock: File,
}

impl Deployer {
    /// The baseline `request`'s application is prepared over, prepared and
    /// published first when this node has none.
    pub(super) async fn baseline(
        &self,
        wasm: &[u8],
        request: &BuildRequest,
        context: &PreparationContext<'_>,
    ) -> anyhow::Result<HeldBaseline> {
        let wanted = request.baseline();
        let identity = wanted.identity()?;
        let name = name(&identity);
        let directory = self.root.join(&name);
        let prepared = directory.join(PREPARED);
        let lock = loop {
            let lock = hold(&directory, Hold::Shared).await?;
            if prepared.exists() {
                break lock;
            }
            drop(lock);
            let _publishing = hold(&directory, Hold::Exclusive).await?;
            if !prepared.exists() {
                let product = context
                    .provider
                    .prepare_runtime_baseline(wasm, &wanted, context.cancellation)
                    .await?;
                // SAFETY: only the host-selected trusted Preparation provider supplies these bytes.
                let artifact = unsafe {
                    context
                        .engine
                        .decode_builder_output(product.prepared_bytes(), identity)
                }?;
                artifact.publish(&prepared)?;
                readable_by_preparation(&directory)?;
                readable_by_preparation(&prepared)?;
            }
        };
        let digest = context.engine.prepared_manifest_sha256(&prepared)?;
        let template = self.base(context.engine, &name, request.runtime.digest()?)?;
        // SAFETY: this deployer publishes only controlled local compiler output in its
        // operator-owned root and never modifies a published baseline.
        let published = unsafe { PublishedBaseline::new(prepared, digest) };
        Ok(HeldBaseline {
            name,
            published,
            template,
            _lock: lock,
        })
    }

    /// The baseline `name` loaded in `engine`, loaded again when this node
    /// holds none or a read of its storage failed.
    pub(super) fn base(
        &self,
        engine: &IsoplaneEngine,
        name: &str,
        runtime_profile: [u8; 32],
    ) -> anyhow::Result<Template> {
        let mut bases = self.baselines.0.lock();
        if let Some(base) = bases.get(name).filter(|base| !base.storage_failed()) {
            return Ok(base.clone());
        }
        let prepared = self.root.join(name).join(PREPARED);
        let digest = engine.prepared_manifest_sha256(&prepared)?;
        // SAFETY: as for `Deployer::restore_prepared`; baselines are published once and never
        // modified.
        let base = unsafe { engine.restore_prepared(&prepared, digest, runtime_profile, &[]) }?;
        bases.insert(name.to_owned(), base.clone());
        Ok(base)
    }
}

/// The baseline a layered version's artifact directory names, if any.
pub(super) fn named_by(artifact: &Path) -> anyhow::Result<Option<String>> {
    let name = match std::fs::read_to_string(artifact.join(BASELINE_FILE)) {
        Ok(name) => name,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        name.strip_prefix(PREFIX)
            .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())),
        "an artifact names a malformed baseline"
    );
    Ok(Some(name))
}

fn name(identity: &isoplane_engine::prepared::PreparedIdentity) -> String {
    let mut hash = blake3::Hasher::new();
    for part in [
        &identity.input_hash,
        &identity.runtime_profile,
        &identity.initialization_hash,
    ] {
        hash.update(part);
    }
    format!("{PREFIX}{}", hash.finalize().to_hex())
}

#[derive(Clone, Copy)]
enum Hold {
    Shared,
    Exclusive,
}

/// Takes the baseline in `directory` as `hold`, creating its directory and
/// lock file when absent. Retries if the lock's path changed while waiting.
async fn hold(directory: &Path, hold: Hold) -> anyhow::Result<File> {
    loop {
        std::fs::create_dir_all(directory)?;
        let lock = match std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(directory.join(LOCK))
        {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let lock = acquire(lock, hold).await?;
        if is_current(&lock, directory)? {
            return Ok(lock);
        }
    }
}

/// Whether `lock` is still the lock file of the baseline in `directory`.
fn is_current(lock: &File, directory: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let held = lock.metadata()?;
    match std::fs::metadata(directory.join(LOCK)) {
        Ok(current) => Ok(held.dev() == current.dev() && held.ino() == current.ino()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Takes `lock` as `hold`, waiting off the async runtime for its holders.
async fn acquire(lock: File, hold: Hold) -> anyhow::Result<File> {
    Ok(tokio::task::spawn_blocking(move || {
        match hold {
            Hold::Shared => fs4::FileExt::lock_shared(&lock),
            Hold::Exclusive => fs4::FileExt::lock(&lock),
        }
        .map(|()| lock)
    })
    .await??)
}

/// Lets the preparation container's unprivileged user list and read a
/// published baseline it mounts.
#[expect(
    clippy::disallowed_methods,
    reason = "the deploy owns the permissions of the baselines it publishes"
)]
fn readable_by_preparation(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(root: &Path, key: &str, baseline: Option<&str>) {
        let directory = root.join(key);
        std::fs::create_dir(&directory).expect("artifact directory");
        if let Some(name) = baseline {
            std::fs::write(directory.join(BASELINE_FILE), name).expect("baseline reference");
        }
    }

    #[test]
    fn a_malformed_baseline_reference_is_refused() {
        let root = tempfile::tempdir().expect("registry");
        artifact(root.path(), "artifact-layer", Some("../elsewhere"));
        assert!(
            named_by(&root.path().join("artifact-layer")).is_err(),
            "a baseline reference must name only a baseline directory"
        );
    }
}
