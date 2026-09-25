//! Process-safe retention for regenerable local artifacts.
#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Target disk budget; active readers can temporarily prevent eviction.
#[derive(Clone, Copy)]
pub struct CacheBytes(pub u64);

/// Default disk budget for one project's compiled artifacts.
pub const PROJECT_CACHE_BYTES: CacheBytes = CacheBytes(5 * 1024 * 1024 * 1024);

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
}

/// Keeps an entry alive across processes until its last reader releases it.
pub struct Lease {
    file: Option<File>,
    directory: PathBuf,
    cache: Cache,
}

impl Cache {
    /// Open private storage. Only entries created by this cache are eligible for eviction.
    pub fn new(root: PathBuf, budget: CacheBytes) -> Result<Self, Error> {
        directory(&root)?;
        Ok(Self { root, budget })
    }

    fn catalog(&self) -> Result<File, Error> {
        let path = self.root.join(".storage.lock");
        let file = open_lock(&path)?;
        fs4::FileExt::lock(&file).map_err(|error| io(&path, error))?;
        Ok(file)
    }

    /// Lease an entry and reclaim old inactive entries automatically.
    pub fn lease(&self, key: CacheKey) -> Result<Lease, Error> {
        self.lease_at(key, SystemTime::now())
    }

    fn lease_at(&self, key: CacheKey, now: SystemTime) -> Result<Lease, Error> {
        let gate = self.catalog()?;
        let name: String = key.0.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = self.root.join(name);
        let marker = path.join(".lease");
        if path.exists() && !marker.is_file() {
            return Err(Error::Unowned(path));
        }
        directory(&path)?;
        let file = open_lock(&marker)?;
        fs4::FileExt::lock_shared(&file).map_err(|error| io(&marker, error))?;
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
    pub fn prune(&self) -> Result<(), Error> {
        let _gate = self.catalog()?;
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
            let marker = path.join(".lease");
            let file = open_lock(&marker)?;
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => {}
                Err(fs4::TryLockError::WouldBlock) => continue,
                Err(fs4::TryLockError::Error(error)) => return Err(io(&marker, error)),
            }
            // The catalog excludes new readers while Windows requires closing before deletion.
            drop(file);
            fs::remove_dir_all(&path).map_err(|error| io(&path, error))?;
            total -= bytes;
        }
        Ok(())
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
        drop(self.file.take());
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

    #[test]
    fn another_cache_owner_cannot_evict_a_live_reader() {
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
