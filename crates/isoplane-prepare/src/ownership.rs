use parking_lot::Mutex;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn active() -> &'static Mutex<HashSet<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(Mutex::default)
}

/// Protects a preparation directory from cleanup in this process and other processes.
pub struct PreparationOwner {
    file: Option<File>,
    directory: PathBuf,
}

impl PreparationOwner {
    /// Create the ownership marker in a newly allocated preparation directory.
    /// Fails if the marker already exists or another process claims it first.
    pub fn create(directory: &Path) -> io::Result<Self> {
        Self::acquire(directory, true)?.ok_or_else(|| io::ErrorKind::WouldBlock.into())
    }

    /// Claim an existing preparation directory for cleanup, or return `None` while owned.
    /// Missing or inaccessible directories and markers return an I/O error.
    pub fn try_acquire(directory: &Path) -> io::Result<Option<Self>> {
        Self::acquire(directory, false)
    }

    fn acquire(directory: &Path, create: bool) -> io::Result<Option<Self>> {
        let directory = directory.canonicalize()?;
        let mut active = active().lock();
        // NFS may grant another exclusive lock to the same process.
        if active.contains(&directory) {
            return Ok(None);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(create)
            .open(directory.join("owner.lock"))?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {}
            Err(fs4::TryLockError::WouldBlock) => return Ok(None),
            Err(fs4::TryLockError::Error(error)) => return Err(error),
        }
        active.insert(directory.clone());
        Ok(Some(Self {
            file: Some(file),
            directory,
        }))
    }
}

impl Drop for PreparationOwner {
    fn drop(&mut self) {
        let mut active = active().lock();
        if let Some(file) = self.file.take() {
            // Unlocked here and not left to the close: a child process spawned while this owner
            // lived keeps the open file, and with it the lock, until it executes its program.
            // If unlocking fails, the close that follows still releases a lock no child shares.
            drop(fs4::FileExt::unlock(&file));
        }
        active.remove(&self.directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_released_owner_can_be_replaced_while_a_child_process_holds_its_lock_file() {
        let directory = tempfile::tempdir().unwrap();
        let owner = PreparationOwner::create(directory.path()).unwrap();
        // What a forked child holds until it executes: the same open file, and so the same lock.
        let inherited = owner.file.as_ref().unwrap().try_clone().unwrap();
        drop(owner);
        assert!(
            PreparationOwner::try_acquire(directory.path()).unwrap().is_some(),
            "a released preparation must be claimable while another holder of its lock file is open"
        );
        drop(inherited);
    }

    #[test]
    fn cleanup_cannot_claim_a_live_preparation_in_the_same_process() {
        let directory = tempfile::tempdir().unwrap();
        let owner = PreparationOwner::create(directory.path()).unwrap();
        assert!(PreparationOwner::try_acquire(directory.path())
            .unwrap()
            .is_none());
        drop(owner);
        assert!(PreparationOwner::try_acquire(directory.path())
            .unwrap()
            .is_some());
    }
}
