//! Owns disposable published artifacts, on storage that can delete a mapped file.

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
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
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
