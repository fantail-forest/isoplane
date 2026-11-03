//! Atomic application selection and ownership of its immutable backing artifacts.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use isoplane_control::private_files;
use isoplane_prepare::PreparationOwner;
use serde::{Deserialize, Serialize};

use crate::deploy::PreparedReceipt;

const MAX_SELECTION_BYTES: usize = 1024 * 1024;

/// A local installation could not be read, selected or reclaimed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// Local files do not meet the operator storage protection requirements.
    #[error(transparent)]
    Protection(#[from] private_files::PrivateFileError),
    /// A filesystem operation failed without establishing absence.
    #[error("{operation} {}: {source}", path.display())]
    Io {
        /// The failed operation.
        operation: &'static str,
        /// The affected path.
        path: PathBuf,
        /// The operating system failure.
        source: std::io::Error,
    },
    /// The application has not been deployed on this host.
    #[error("application `{0}` is not installed; deploy its bundle before starting the daemon")]
    Missing(String),
    /// Existing selection is corrupt; it must not authorize garbage collection.
    #[error("invalid installed application record {}: {source}", path.display())]
    Record {
        /// The affected record.
        path: PathBuf,
        /// The encoding failure.
        source: serde_json::Error,
    },
    /// A record exceeds its bounded serialization format.
    #[error("installed application record exceeds one MiB: {}", .0.display())]
    TooLarge(PathBuf),
    /// A record does not belong to its expected local owner or application.
    #[error("installed application identity does not match `{0}`")]
    Identity(String),
    /// Another process owns a mutually exclusive operation.
    #[error("another process owns {0}")]
    Busy(String),
    /// Selection is visible, but its survival across machine failure is uncertain.
    #[error("application `{app}` was selected, but flushing its selection failed: {source}")]
    Durability {
        /// The selected application.
        app: String,
        /// The filesystem flush failure.
        source: std::io::Error,
    },
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> InstallError {
    InstallError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

fn lock_file(path: &Path, wait: bool) -> Result<File, InstallError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|e| io("opening lock", path, e))?;
    if wait {
        fs4::FileExt::lock(&file).map_err(|e| io("locking", path, e))?;
    } else {
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(fs4::TryLockError::WouldBlock) => {
                return Err(InstallError::Busy(path.display().to_string()));
            }
            Err(fs4::TryLockError::Error(error)) => {
                return Err(io("locking", path, error));
            }
        }
    }
    Ok(file)
}

struct Transaction {
    _local: parking_lot::MutexGuard<'static, ()>,
    _file: File,
}

fn transaction(registry: &Path) -> Result<Transaction, InstallError> {
    static LOCAL: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let local = LOCAL.lock();
    Ok(Transaction {
        _local: local,
        _file: lock_file(&registry.join("store.lock"), true)?,
    })
}

pub(crate) fn new_publication(
    registry: &Path,
) -> Result<(tempfile::TempDir, PreparationOwner), InstallError> {
    let _transaction = transaction(registry)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix("artifact-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let directory = builder
        .tempdir_in(registry)
        .map_err(|error| io("creating publication", registry, error))?;
    let owner = PreparationOwner::create(directory.path())
        .map_err(|error| io("owning publication", directory.path(), error))?;
    Ok((directory, owner))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    owner: String,
    receipt: PreparedReceipt,
}

/// The selected artifact and the lease keeping its recovery pathname available.
pub struct InstalledApplication {
    /// Preparation reference selected for the application.
    pub receipt: PreparedReceipt,
    _lease: PreparationOwner,
}

impl InstalledApplication {
    pub(crate) fn new(receipt: PreparedReceipt, lease: PreparationOwner) -> Self {
        Self {
            receipt,
            _lease: lease,
        }
    }

    pub(crate) fn into_parts(self) -> (PreparedReceipt, std::sync::Arc<PreparationOwner>) {
        (self.receipt, std::sync::Arc::new(self._lease))
    }
}

/// One current selection per application; retired artifacts have no historical records.
pub struct InstalledApplications {
    root: PathBuf,
    selected: PathBuf,
    registry: PathBuf,
}

impl InstalledApplications {
    /// Opens protected local state, creating empty directories when absent.
    pub fn open(root: &Path) -> Result<Self, InstallError> {
        private_files::directory(root)?;
        let selected = root.join("selected");
        let registry = root.join("registry");
        private_files::directory(&selected)?;
        Ok(Self {
            root: root.to_owned(),
            selected,
            registry,
        })
    }

    /// Excludes another daemon using the same application state until dropped.
    pub fn daemon(&self) -> Result<File, InstallError> {
        self.exclusive("daemon.lock", false)
    }

    /// Serializes local deployment producers without blocking a serving daemon.
    pub fn deployment(&self) -> Result<File, InstallError> {
        self.exclusive("deploy.lock", false)
    }

    fn exclusive(&self, name: &str, wait: bool) -> Result<File, InstallError> {
        lock_file(&self.root.join(name), wait)
    }

    fn transaction(&self) -> Result<Transaction, InstallError> {
        transaction(&self.registry)
    }

    fn path(&self, app: &str) -> PathBuf {
        self.selected
            .join(format!("{}.json", blake3::hash(app.as_bytes()).to_hex()))
    }

    fn read(&self, path: &Path) -> Result<Selection, InstallError> {
        let bytes = private_files::read(path, MAX_SELECTION_BYTES + 1)?;
        if bytes.len() > MAX_SELECTION_BYTES {
            return Err(InstallError::TooLarge(path.to_owned()));
        }
        serde_json::from_slice(&bytes).map_err(|source| InstallError::Record {
            path: path.to_owned(),
            source,
        })
    }

    /// Reads and pins a complete startup selection before cleanup can retire its files.
    pub fn load(
        &self,
        owner: &str,
        apps: &[&str],
    ) -> Result<Vec<InstalledApplication>, InstallError> {
        let _transaction = self.transaction()?;
        apps.iter()
            .map(|app| {
                let selection = match self.read(&self.path(app)) {
                    Err(InstallError::Protection(private_files::PrivateFileError::Missing(_))) => {
                        return Err(InstallError::Missing((*app).to_owned()));
                    }
                    result => result?,
                };
                if selection.owner != owner || selection.receipt.manifest.name != *app {
                    return Err(InstallError::Identity((*app).to_owned()));
                }
                let directory = self.artifact(&selection.receipt);
                let lease = PreparationOwner::try_acquire(&directory)
                    .map_err(|e| io("pinning installed artifact", &directory, e))?
                    .ok_or_else(|| InstallError::Busy(directory.display().to_string()))?;
                Ok(InstalledApplication {
                    receipt: selection.receipt,
                    _lease: lease,
                })
            })
            .collect()
    }

    fn artifact(&self, receipt: &PreparedReceipt) -> PathBuf {
        self.registry
            .join(String::from(receipt.artifact_key.clone()))
    }

    /// Atomically selects a publication while its producer still owns its backing files.
    pub fn select(
        &self,
        owner: &str,
        prepared: &crate::deploy::PreparedInstallation,
    ) -> Result<(), InstallError> {
        self.select_receipt(owner, &prepared.receipt)
    }

    fn select_receipt(&self, owner: &str, receipt: &PreparedReceipt) -> Result<(), InstallError> {
        let _transaction = self.transaction()?;
        let app = &receipt.manifest.name;
        if owner.trim().is_empty() || app.trim().is_empty() {
            return Err(InstallError::Identity(app.clone()));
        }
        let path = self.path(app);
        match self.read(&path) {
            Ok(old) if old.owner != owner || old.receipt.manifest.name != *app => {
                return Err(InstallError::Identity(app.clone()));
            }
            Ok(_) | Err(InstallError::Protection(private_files::PrivateFileError::Missing(_))) => {}
            Err(error) => return Err(error),
        }
        let selection = Selection {
            owner: owner.to_owned(),
            receipt: receipt.clone(),
        };
        let bytes = serde_json::to_vec(&selection).map_err(|source| InstallError::Record {
            path: path.clone(),
            source,
        })?;
        if bytes.len() > MAX_SELECTION_BYTES {
            return Err(InstallError::TooLarge(path));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.selected)
            .map_err(|e| io("creating selection", &path, e))?;
        temporary
            .write_all(&bytes)
            .map_err(|e| io("writing selection", &path, e))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|e| io("flushing selection", &path, e))?;
        temporary
            .persist(&path)
            .map_err(|e| io("selecting application", &path, e.error))?;
        File::open(&self.selected)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| InstallError::Durability {
                app: app.clone(),
                source,
            })
    }

    /// Retires applications removed from a successfully restored host configuration.
    pub fn retain_configured(&self, owner: &str, apps: &[&str]) -> Result<(), InstallError> {
        let _transaction = self.transaction()?;
        let mut obsolete = Vec::new();
        for entry in std::fs::read_dir(&self.selected)
            .map_err(|e| io("reading selections", &self.selected, e))?
        {
            let path = entry
                .map_err(|e| io("reading selection", &self.selected, e))?
                .path();
            if !path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                continue;
            }
            let selection = self.read(&path)?;
            let app = &selection.receipt.manifest.name;
            if selection.owner != owner || self.path(app) != path {
                return Err(InstallError::Identity(app.clone()));
            }
            if !apps.contains(&app.as_str()) {
                obsolete.push(path);
            }
        }
        for path in obsolete {
            std::fs::remove_file(&path)
                .map_err(|e| io("retiring removed application", &path, e))?;
        }
        File::open(&self.selected)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| io("flushing selections", &self.selected, e))
    }

    /// Removes only unselected app artifacts whose producer and execution leases have ended.
    /// Shared runtime baselines and the uninitialized compiler cache remain reusable.
    pub fn collect(&self) -> Result<(), InstallError> {
        let _transaction = self.transaction()?;
        let mut selected = HashSet::new();
        for entry in std::fs::read_dir(&self.selected)
            .map_err(|e| io("reading selections", &self.selected, e))?
        {
            let path = entry
                .map_err(|e| io("reading selection", &self.selected, e))?
                .path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let selection = self.read(&path)?;
                if self.path(&selection.receipt.manifest.name) != path {
                    return Err(InstallError::Identity(selection.receipt.manifest.name));
                }
                selected.insert(String::from(selection.receipt.artifact_key));
            }
        }
        for entry in std::fs::read_dir(&self.registry)
            .map_err(|e| io("reading artifacts", &self.registry, e))?
        {
            let entry = entry.map_err(|e| io("reading artifact", &self.registry, e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with("artifact-") || selected.contains(name) {
                continue;
            }
            let directory = entry.path();
            let lease =
                match PreparationOwner::try_acquire(&directory) {
                    Ok(lease) => lease,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        // Allocation and marker creation share this transaction, so no live producer
                        // can still be creating this marker; a crash abandoned the directory first.
                        Some(PreparationOwner::create(&directory).map_err(|error| {
                            io("claiming abandoned publication", &directory, error)
                        })?)
                    }
                    Err(error) => return Err(io("claiming retired artifact", &directory, error)),
                };
            let Some(_lease) = lease else { continue };
            std::fs::remove_dir_all(&directory)
                .map_err(|e| io("removing retired artifact", &directory, e))?;
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::deploy::ArtifactKey;

    fn store() -> (tempfile::TempDir, InstalledApplications) {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let store = InstalledApplications::open(root.path()).unwrap();
        private_files::directory(&store.registry).unwrap();
        (root, store)
    }

    fn publication(
        store: &InstalledApplications,
        app: &str,
        version: &str,
    ) -> (PreparedReceipt, PreparationOwner, PathBuf) {
        let directory = tempfile::Builder::new()
            .prefix("artifact-")
            .tempdir_in(&store.registry)
            .unwrap();
        let lease = PreparationOwner::create(directory.path()).unwrap();
        let path = directory.keep();
        let receipt = PreparedReceipt {
            version: version.into(),
            artifact_key: ArtifactKey::try_from(
                path.file_name().unwrap().to_str().unwrap().to_owned(),
            )
            .unwrap(),
            manifest_sha256: "00".repeat(32),
            settings_digest: "11".repeat(32),
            runtime_profile: isoplane_prepare::RuntimeProfile::RawWasm,
            manifest: toml::from_str(&format!("name = {app:?}")).unwrap(),
        };
        (receipt, lease, path)
    }

    #[test]
    fn replacement_retains_only_the_selected_and_live_backing_artifacts() {
        let (_root, store) = store();
        let (old, producer, old_path) = publication(&store, "one", "first");
        store.select_receipt("owner", &old).unwrap();
        drop(producer);
        let running = store.load("owner", &["one"]).unwrap();
        let (second, producer, second_path) = publication(&store, "one", "second");
        store.select_receipt("owner", &second).unwrap();
        drop(producer);
        let (third, producer, third_path) = publication(&store, "one", "third");
        store.select_receipt("owner", &third).unwrap();
        drop(producer);
        store.collect().unwrap();
        assert!(
            old_path.is_dir(),
            "the active recovery pathname must remain available"
        );
        assert!(
            !second_path.exists(),
            "unused intermediate deployment is not history"
        );
        assert!(
            third_path.is_dir(),
            "selection survives without a running daemon"
        );
        drop(running);
        store.collect().unwrap();
        assert!(!old_path.exists(), "retirement follows the last live owner");
        assert_eq!(
            store.load("owner", &["one"]).unwrap()[0].receipt.version,
            "third"
        );
    }

    #[test]
    fn abandoned_preparation_cannot_replace_or_reclaim_another_apps_selection() {
        let (_root, store) = store();
        let (first, owner, first_path) = publication(&store, "one", "first");
        store.select_receipt("owner", &first).unwrap();
        drop(owner);
        let before = std::fs::read(store.path("one")).unwrap();
        let (_failed, producer, failed_path) = publication(&store, "two", "failed");
        store.collect().unwrap();
        assert!(
            failed_path.exists(),
            "a producer owns its unpublished output"
        );
        drop(producer);
        store.collect().unwrap();
        assert!(!failed_path.exists());
        assert!(first_path.exists());
        assert_eq!(std::fs::read(store.path("one")).unwrap(), before);
        assert!(matches!(
            store.load("owner", &["two"]),
            Err(InstallError::Missing(_))
        ));
    }

    #[test]
    fn corrupt_selection_refuses_both_replacement_and_garbage_collection() {
        let (_root, store) = store();
        let (first, producer, first_path) = publication(&store, "one", "first");
        store.select_receipt("owner", &first).unwrap();
        drop(producer);
        std::fs::write(store.path("one"), b"broken record").unwrap();
        let (next, producer, next_path) = publication(&store, "one", "next");
        assert!(matches!(
            store.select_receipt("owner", &next),
            Err(InstallError::Record { .. })
        ));
        drop(producer);
        assert!(matches!(store.collect(), Err(InstallError::Record { .. })));
        assert!(first_path.exists() && next_path.exists());
        assert_eq!(std::fs::read(store.path("one")).unwrap(), b"broken record");
    }

    #[test]
    fn oversized_records_and_owner_changes_are_refused_without_replacement() {
        let (_root, store) = store();
        let (receipt, _producer, _path) = publication(&store, "one", "first");
        store.select_receipt("owner", &receipt).unwrap();
        let before = std::fs::read(store.path("one")).unwrap();
        assert!(matches!(
            store.select_receipt("other", &receipt),
            Err(InstallError::Identity(_))
        ));
        assert_eq!(std::fs::read(store.path("one")).unwrap(), before);
        let mut oversized = before;
        oversized.resize(1024 * 1024, b' ');
        oversized.push(b'x');
        std::fs::write(store.path("one"), oversized).unwrap();
        assert!(matches!(store.collect(), Err(InstallError::TooLarge(_))));
    }

    #[test]
    fn oversized_replacements_preserve_readable_selections_and_collection() {
        fn sized(mut receipt: PreparedReceipt, bytes: usize) -> PreparedReceipt {
            receipt.manifest.init_visible_vars = vec![("padding".into(), String::new())];
            let overhead = serde_json::to_vec(&Selection {
                owner: "owner".into(),
                receipt: receipt.clone(),
            })
            .unwrap()
            .len();
            receipt.manifest.init_visible_vars[0].1 = "x".repeat(bytes - overhead);
            receipt
        }

        let (_root, store) = store();
        let (first, producer, first_path) = publication(&store, "one", "first");
        let first = sized(first, 1024 * 1024);
        store.select_receipt("owner", &first).unwrap();
        drop(producer);
        let before = std::fs::read(store.path("one")).unwrap();
        assert_eq!(before.len(), 1024 * 1024);
        assert_eq!(
            store.load("owner", &["one"]).unwrap()[0].receipt.version,
            "first"
        );

        let (sibling, producer, sibling_path) = publication(&store, "two", "sibling");
        store.select_receipt("owner", &sibling).unwrap();
        drop(producer);
        let sibling_before = std::fs::read(store.path("two")).unwrap();
        let (next, producer, next_path) = publication(&store, "one", "next");
        let next = sized(next, 1024 * 1024 + 1);
        assert!(matches!(
            store.select_receipt("owner", &next),
            Err(InstallError::TooLarge(_))
        ));
        assert_eq!(std::fs::read(store.path("one")).unwrap(), before);
        assert_eq!(std::fs::read(store.path("two")).unwrap(), sibling_before);
        drop(producer);
        store.collect().unwrap();
        assert!(first_path.is_dir() && sibling_path.is_dir());
        assert!(!next_path.exists());
        assert_eq!(
            store.load("owner", &["one"]).unwrap()[0].receipt.version,
            "first"
        );
    }

    #[test]
    fn configured_app_retirement_preserves_current_apps_and_live_old_files() {
        let (_root, store) = store();
        let (first, producer, path) = publication(&store, "one", "first");
        store.select_receipt("owner", &first).unwrap();
        drop(producer);
        let running = store.load("owner", &["one"]).unwrap();
        store.retain_configured("owner", &[]).unwrap();
        store.collect().unwrap();
        assert!(path.exists());
        assert!(matches!(
            store.load("owner", &["one"]),
            Err(InstallError::Missing(_))
        ));
        drop(running);
        store.collect().unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn collection_reclaims_a_crash_before_marker_creation_but_never_an_allocated_owner() {
        let (_root, store) = store();
        let abandoned = store.registry.join("artifact-crashed");
        {
            let _transaction = store.transaction().unwrap();
            std::fs::create_dir(&abandoned).unwrap();
        }
        let (active, owner) = new_publication(&store.registry).unwrap();
        store.collect().unwrap();
        assert!(!abandoned.exists());
        assert!(active.path().is_dir());
        let path = active.keep();
        drop(owner);
        store.collect().unwrap();
        assert!(!path.exists());
    }
}
