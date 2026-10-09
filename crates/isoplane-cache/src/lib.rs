//! Process-safe retention for regenerable local artifacts.
#![forbid(unsafe_code)]

use parking_lot::{Mutex, MutexGuard};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::SystemTime;

/// Target disk budget; active readers can temporarily prevent eviction.
#[derive(Clone, Copy)]
pub struct CacheBytes(pub u64);

/// Default disk budget for one project's compiled artifacts.
pub const PROJECT_CACHE_BYTES: CacheBytes = CacheBytes(5 * 1024 * 1024 * 1024);

/// Disk budget for the compiled shared runtimes one user's projects reuse.
/// A runtime's entry is about 230 MB, so this holds the ordinary and debug
/// runtimes of the SDK in use and of the one before it.
pub const RUNTIME_CACHE_BYTES: CacheBytes = CacheBytes(1024 * 1024 * 1024);

/// Content identity of one independently evictable artifact directory.
#[derive(Clone, Copy)]
pub struct CacheKey(pub [u8; 32]);

/// Failure to establish or maintain owned cache storage.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation failed at the named cache path.
    #[error("cache operation at {path}: {source}")]
    Io {
        /// Path being accessed.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: std::io::Error,
    },
    /// The selected path cannot safely be managed as cache storage.
    #[error("cache path is not an owned regular directory or file: {0}")]
    Unowned(PathBuf),
}

fn io(path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

/// A byte-bounded collection of content-keyed artifact directories.
#[derive(Clone)]
pub struct Cache {
    root: PathBuf,
    budget: CacheBytes,
    entries: Arc<LocalEntries>,
}

type LocalEntries = Mutex<HashMap<PathBuf, Weak<File>>>;

/// Where evicted entries wait until all their files can be deleted.
const EVICTED: &str = ".evicted";

fn local_entries(root: &Path) -> Arc<LocalEntries> {
    static ROOTS: OnceLock<Mutex<HashMap<PathBuf, Weak<LocalEntries>>>> = OnceLock::new();
    let mut roots = ROOTS.get_or_init(Mutex::default).lock();
    roots.retain(|_, entries| entries.strong_count() != 0);
    if let Some(entries) = roots.get(root).and_then(Weak::upgrade) {
        return entries;
    }
    let entries = Arc::new(Mutex::default());
    roots.insert(root.into(), Arc::downgrade(&entries));
    entries
}

struct Catalog<'a> {
    // Close the process lock before allowing another local owner to open it.
    _file: File,
    entries: MutexGuard<'a, HashMap<PathBuf, Weak<File>>>,
}

/// Keeps an entry alive across processes until its last reader releases it.
pub struct Lease {
    file: Option<Arc<File>>,
    directory: PathBuf,
    cache: Cache,
}

impl Cache {
    /// Open private storage. Only entries created by this cache are eligible for eviction.
    pub fn new(root: PathBuf, budget: CacheBytes) -> Result<Self, Error> {
        directory(&root)?;
        let root = root.canonicalize().map_err(|error| io(&root, error))?;
        let entries = local_entries(&root);
        Ok(Self {
            root,
            budget,
            entries,
        })
    }

    fn catalog(&self) -> Result<Catalog<'_>, Error> {
        let entries = self.entries.lock();
        let path = self.root.join(".storage.lock");
        let file = open_lock(&path)?;
        fs4::FileExt::lock(&file).map_err(|error| io(&path, error))?;
        Ok(Catalog {
            _file: file,
            entries,
        })
    }

    /// Lease an entry and reclaim old inactive entries automatically.
    pub fn lease(&self, key: CacheKey) -> Result<Lease, Error> {
        self.lease_at(key, SystemTime::now())
    }

    fn lease_at(&self, key: CacheKey, now: SystemTime) -> Result<Lease, Error> {
        let mut gate = self.catalog()?;
        let name: String = key.0.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = self.root.join(name);
        let marker = path.join(".lease");
        if path.exists() && !marker.is_file() {
            return Err(Error::Unowned(path));
        }
        directory(&path)?;
        // NFS locks can belong to a process rather than an individual open handle.
        let file = match gate.entries.get(&path).and_then(Weak::upgrade) {
            Some(file) => file,
            None => {
                let file = Arc::new(open_lock(&marker)?);
                fs4::FileExt::lock_shared(file.as_ref()).map_err(|error| io(&marker, error))?;
                gate.entries.insert(path.clone(), Arc::downgrade(&file));
                file
            }
        };
        file.set_modified(now).map_err(|error| io(&marker, error))?;
        drop(gate);
        let lease = Lease {
            file: Some(file),
            directory: path,
            cache: self.clone(),
        };
        self.prune()?;
        Ok(lease)
    }

    /// Evict least-recently-used inactive entries until the budget is met.
    /// Active entries may exceed the budget; releasing their leases retries eviction.
    /// An evicted entry frees its key at once. Files the filesystem cannot
    /// delete yet are deleted by a later call.
    pub fn prune(&self) -> Result<(), Error> {
        let mut gate = self.catalog()?;
        gate.entries.retain(|_, file| file.strong_count() != 0);
        self.delete_evicted_entries()?;
        let entries = self.root.clone();
        let mut inventory = Vec::new();
        for entry in fs::read_dir(&entries).map_err(|error| io(&entries, error))? {
            let path = entry.map_err(|error| io(&entries, error))?.path();
            let marker = path.join(".lease");
            if path.is_symlink() || !path.is_dir() || marker.is_symlink() || !marker.is_file() {
                continue;
            }
            let touched = fs::metadata(&marker)
                .and_then(|metadata| metadata.modified())
                .map_err(|error| io(&marker, error))?;
            inventory.push((touched, path.clone(), size(&path)?));
        }
        let mut total: u64 = inventory.iter().map(|(_, _, bytes)| bytes).sum();
        inventory.sort();
        for (_, path, bytes) in inventory {
            if total <= self.budget.0 {
                break;
            }
            if gate.entries.contains_key(&path) {
                continue;
            }
            let marker = path.join(".lease");
            let file = open_lock(&marker)?;
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => {}
                Err(fs4::TryLockError::WouldBlock) => continue,
                Err(fs4::TryLockError::Error(error)) => return Err(io(&marker, error)),
            }
            // The catalog excludes new readers while Windows requires closing before deletion.
            drop(file);
            self.evict(&path)?;
            total -= bytes;
        }
        Ok(())
    }

    /// Moves an entry out of its key before deleting it. Deleting in place
    /// could stop after the lease marker was gone, and a directory without its
    /// marker is refused as unowned by every later lease of that key. NFS
    /// stops there whenever another process still has a file open, such as a
    /// child spawned while the entry was leased.
    fn evict(&self, entry: &Path) -> Result<(), Error> {
        let evicted = self.root.join(EVICTED);
        directory(&evicted)?;
        let mut number = 0_u32;
        let destination = loop {
            let candidate = evicted.join(number.to_string());
            if fs::symlink_metadata(&candidate).is_err() {
                break candidate;
            }
            number += 1;
        };
        fs::rename(entry, &destination).map_err(|error| io(entry, error))?;
        delete_evicted(&destination);
        Ok(())
    }

    /// Retries the deletions earlier evictions could not finish.
    fn delete_evicted_entries(&self) -> Result<(), Error> {
        let evicted = self.root.join(EVICTED);
        if evicted.is_symlink() {
            return Err(Error::Unowned(evicted));
        }
        let entries = match fs::read_dir(&evicted) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io(&evicted, error)),
        };
        for entry in entries {
            delete_evicted(&entry.map_err(|error| io(&evicted, error))?.path());
        }
        Ok(())
    }
}

/// Deletes an evicted entry, leaving what cannot be deleted yet for the next attempt.
fn delete_evicted(path: &Path) {
    // macOS keeps a `._name` file beside an entry on filesystems without
    // extended attributes, and deletes it with the entry.
    let deleted = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match deleted {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            tracing::warn!(%error, path = %path.display(), "evicted cache entry is not fully deleted yet");
        }
        Ok(()) | Err(_) => {}
    }
}

impl Lease {
    /// Directory protected from eviction for this lease's lifetime.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        {
            let _gate = self.cache.entries.lock();
            // The last reader unlocks instead of leaving that to the close: a child
            // process spawned while the lease was held keeps the open file, and
            // with it the lock, until it executes its program.
            if let Some(file) = self.file.take().and_then(Arc::into_inner) {
                if let Err(error) = fs4::FileExt::unlock(&file) {
                    tracing::warn!(%error, directory = %self.directory.display(), "artifact cache lease release failed");
                }
            }
        }
        if let Err(error) = self.cache.prune() {
            tracing::warn!(%error, "artifact cache retention failed");
        }
    }
}

fn directory(path: &Path) -> Result<(), Error> {
    if path.is_symlink() {
        return Err(Error::Unowned(path.into()));
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| io(path, error))
}

fn open_lock(path: &Path) -> Result<File, Error> {
    if path.is_symlink() {
        return Err(Error::Unowned(path.into()));
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| io(path, error))
}

fn size(path: &Path) -> Result<u64, Error> {
    match existing_size(path) {
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => Ok(0),
        result => result,
    }
}

fn existing_size(path: &Path) -> Result<u64, Error> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io(path, error))?;
    if metadata.is_symlink() {
        return Ok(0);
    }
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut bytes = 0;
    for entry in fs::read_dir(path).map_err(|error| io(path, error))? {
        bytes += size(&entry.map_err(|error| io(path, error))?.path())?;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn releasing_the_last_reader_enforces_the_budget() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::new(root.path().into(), CacheBytes(0)).unwrap();
        let reader = cache.lease(CacheKey([1; 32])).unwrap();
        let path = reader.directory().to_path_buf();
        fs::write(path.join("module.cwasm"), b"compiled").unwrap();
        cache.prune().unwrap();
        assert!(
            path.exists(),
            "a mapped artifact must survive retention while leased"
        );
        drop(reader);
        assert!(
            !path.exists(),
            "releasing the final reader must reclaim over-budget artifacts automatically"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_child_process_holding_a_released_lease_file_does_not_delay_reclamation() {
        const CHILD: &str = "ISOPLANE_CACHE_TEST_HOLDS_LEASE_FILE";
        if std::env::var_os(CHILD).is_some() {
            // Keeps its inherited copy of the lease file open until the parent closes this pipe.
            std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).unwrap();
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::new(root.path().into(), CacheBytes(0)).unwrap();
        let reader = cache.lease(CacheKey([1; 32])).unwrap();
        let path = reader.directory().to_path_buf();
        fs::write(path.join("module.cwasm"), b"compiled").unwrap();
        // Any process spawned while a lease is held shares its open lease file
        // from its fork until it executes. Passing the file as standard error
        // keeps it open for the child's whole life.
        let inherited = reader.file.as_ref().unwrap().try_clone().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::a_child_process_holding_a_released_lease_file_does_not_delay_reclamation",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(inherited);
        let mut child = command.spawn().unwrap();
        drop(command);
        drop(reader);
        let reclaimed = !path.exists();
        drop(child.stdin.take());
        assert!(
            child.wait().unwrap().success(),
            "the child must hold the lease file until released"
        );
        assert!(
            reclaimed,
            "releasing the final reader must reclaim its artifact while a child process still holds the lease file open"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_that_cannot_be_deleted_yet_frees_its_key() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::new(root.path().into(), CacheBytes(0)).unwrap();
        let reader = cache.lease(CacheKey([1; 32])).unwrap();
        let path = reader.directory().to_path_buf();
        // One of these stops the deletion on any filesystem: a directory that
        // refuses changes where permissions are enforced, and a file that is
        // still open on NFS.
        let sealed = path.join("sealed");
        fs::create_dir(&sealed).unwrap();
        fs::write(sealed.join("module.cwasm"), b"compiled").unwrap();
        let open = File::open(sealed.join("module.cwasm")).unwrap();
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o500)).unwrap();
        drop(reader);
        let waiting = root.path().join(EVICTED).join("0");
        assert!(
            waiting.join("sealed").is_dir() && !path.exists(),
            "an entry whose files cannot all be deleted must wait outside its key"
        );
        let again = cache
            .lease(CacheKey([1; 32]))
            .expect("the key must be leasable while its evicted entry waits for deletion");
        assert!(
            !again.directory().join("sealed").exists(),
            "a new lease of the key must start from an empty entry"
        );
        fs::write(again.directory().join("module.cwasm"), b"compiled").unwrap();
        drop(again);
        assert!(
            !path.exists() && waiting.is_dir() && !waiting.with_file_name("1").exists(),
            "a later eviction must complete beside the one still waiting"
        );
        drop(open);
        fs::set_permissions(waiting.join("sealed"), fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn a_later_prune_deletes_what_an_eviction_left_behind() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::new(root.path().into(), CacheBytes(0)).unwrap();
        let left = root.path().join(EVICTED).join("0");
        fs::create_dir_all(&left).unwrap();
        fs::write(left.join("module.cwasm"), b"compiled").unwrap();
        cache.prune().unwrap();
        assert!(
            !left.exists(),
            "retention must delete an evicted entry an earlier attempt left behind"
        );
    }

    #[test]
    fn another_cache_owner_cannot_evict_a_live_reader() {
        const CHILD_ENTRY: &str = "ISOPLANE_CACHE_TEST_LEASED_ENTRY";
        if let Some(path) = std::env::var_os(CHILD_ENTRY) {
            let path = PathBuf::from(path);
            Cache::new(path.parent().unwrap().into(), CacheBytes(0))
                .unwrap()
                .prune()
                .unwrap();
            assert!(
                path.join("module").is_file(),
                "another process must preserve a live reader"
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::new(root.path().into(), CacheBytes(0)).unwrap();
        let first = cache.lease(CacheKey([1; 32])).unwrap();
        let other = Cache::new(root.path().into(), CacheBytes(0)).unwrap();
        let second = other.lease(CacheKey([1; 32])).unwrap();
        fs::write(first.directory().join("module"), b"compiled").unwrap();
        drop(first);
        assert!(
            second.directory().exists(),
            "a second independent owner must retain its mapped artifact"
        );
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::another_cache_owner_cannot_evict_a_live_reader",
                "--nocapture",
            ])
            .env(CHILD_ENTRY, second.directory())
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        let path = second.directory().to_path_buf();
        drop(second);
        assert!(
            !path.exists(),
            "the final local reader releases the cross-process lease"
        );
    }

    #[test]
    fn least_recently_used_artifacts_are_evicted_first() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::new(root.path().into(), CacheBytes(8)).unwrap();
        let old = cache
            .lease_at(CacheKey([1; 32]), UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        let path = old.directory().to_path_buf();
        fs::write(path.join("module"), b"1234").unwrap();
        drop(old);
        let new = cache
            .lease_at(CacheKey([2; 32]), UNIX_EPOCH + Duration::from_secs(2))
            .unwrap();
        fs::write(new.directory().join("module"), b"12345").unwrap();
        cache.prune().unwrap();
        assert!(
            !path.exists(),
            "the least recently used inactive artifact must make room for the new one"
        );
        assert!(
            new.directory().exists(),
            "retention must preserve the new live artifact"
        );
    }

    #[test]
    fn unknown_directories_are_not_adopted_by_retention() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::new(root.path().into(), CacheBytes(0)).unwrap();
        let unknown = root.path().join("customer");
        fs::create_dir(&unknown).unwrap();
        fs::write(unknown.join("data"), b"keep").unwrap();
        cache.prune().unwrap();
        assert!(
            unknown.join("data").exists(),
            "retention must leave unowned directories untouched"
        );
    }
}
