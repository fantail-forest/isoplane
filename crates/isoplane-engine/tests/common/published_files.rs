//! Owns disposable published artifacts, including Windows read-only payloads.

use std::io;
use std::path::{Path, PathBuf};

/// A test directory whose published payloads are removed during teardown.
pub struct Directory(PathBuf);

/// Creates uniquely owned test data where mapped-file deletion is supported.
pub fn tempdir() -> io::Result<Directory> {
    #[cfg(windows)]
    let directory = {
        // SMB can refuse unlinking an open mapping even with delete sharing.
        // These small test publications need native filesystem semantics.
        let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Windows LOCALAPPDATA is unavailable",
            )
        })?;
        tempfile::Builder::new()
            .prefix("isoplane-publication-test-")
            .tempdir_in(Path::new(&local).join("Temp"))?
    };
    #[cfg(target_os = "macos")]
    let directory = {
        // NFS retains unlinked mappings as .nfs files, preventing directory removal.
        tempfile::Builder::new()
            .prefix("isoplane-publication-test-")
            .tempdir_in("/tmp")?
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let directory = tempfile::tempdir()?;
    Ok(Directory(directory.keep()))
}

impl Directory {
    /// Returns the root containing this test's artifacts.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Err(error) = remove_dir_all(&self.0) {
            if std::thread::panicking() {
                eprintln!(
                    "failed to remove test artifacts at {}: {error}",
                    self.0.display()
                );
            } else {
                panic!(
                    "failed to remove test artifacts at {}: {error}",
                    self.0.display()
                );
            }
        }
    }
}

/// Unlinks a test payload, clearing its Windows read-only attribute if needed.
pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    #[cfg(windows)]
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "Windows-only: clearing the DOS attribute cannot change Unix permission bits"
    )]
    {
        // Windows requires clearing the attribute even when unlinking, unlike Unix modes.
        let mut permissions = std::fs::symlink_metadata(path)?.permissions();
        if permissions.readonly() {
            permissions.set_readonly(false);
            std::fs::set_permissions(path, permissions)?;
        }
    }
    std::fs::remove_file(path)
}

/// Removes a test artifact tree without following directory symlinks.
pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_dir_all(entry.path())?;
        } else {
            remove_file(entry.path())?;
        }
    }
    std::fs::remove_dir(path)
}
