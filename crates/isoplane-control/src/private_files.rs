//! Owner-only local files used by application settings and selected deployment state.

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
use std::path::{Path, PathBuf};

/// A protected local file could not be acquired without weakening its protection.
#[derive(Debug, thiserror::Error)]
pub enum PrivateFileError {
    /// A required dependency is absent.
    #[error("required private path is missing: {}", .0.display())]
    Missing(PathBuf),
    /// The operator must repair file ownership, permissions or file type.
    #[error("private path must be owned by this user, accessible only to its owner and contain no links: {}", .0.display())]
    Protection(PathBuf),
    /// An existing dependency could not be read or created.
    #[error("accessing private path {}: {source}", path.display())]
    Io {
        /// The selected local path, without its contents.
        path: PathBuf,
        /// The operating system failure.
        source: std::io::Error,
    },
    /// This build cannot enforce the required local protection.
    #[error("protected operator files require Unix filesystem ownership and modes")]
    Unsupported,
}

fn io(path: &Path, source: std::io::Error) -> PrivateFileError {
    if source.kind() == std::io::ErrorKind::NotFound {
        PrivateFileError::Missing(path.to_owned())
    } else {
        PrivateFileError::Io {
            path: path.to_owned(),
            source,
        }
    }
}

#[cfg(unix)]
fn protected(
    path: &Path,
    metadata: &std::fs::Metadata,
    directory: bool,
) -> Result<(), PrivateFileError> {
    use std::os::unix::fs::MetadataExt;
    let correct_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file() && metadata.nlink() == 1
    };
    if !correct_type
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7077 != 0
    {
        return Err(PrivateFileError::Protection(path.to_owned()));
    }
    Ok(())
}

/// Creates an owner-only directory or verifies the existing directory.
#[cfg(unix)]
pub fn directory(path: &Path) -> Result<(), PrivateFileError> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(io(path, error)),
    };
    let metadata = std::fs::symlink_metadata(path).map_err(|error| io(path, error))?;
    protected(path, &metadata, true)?;
    Ok(())
}

/// Refuses files on platforms without the supported ownership model.
#[cfg(not(unix))]
pub fn directory(_path: &Path) -> Result<(), PrivateFileError> {
    Err(PrivateFileError::Unsupported)
}

/// Opens a regular owner-only file without following a final symlink and reads at most `limit` bytes.
#[cfg(unix)]
pub fn read(path: &Path, limit: usize) -> Result<Vec<u8>, PrivateFileError> {
    use std::os::unix::fs::OpenOptionsExt;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| io(path, error))?;
    protected(path, &metadata, false)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        // A FIFO substituted after the path check must reach descriptor validation without blocking.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| io(path, error))?;
    protected(
        path,
        &file.metadata().map_err(|error| io(path, error))?,
        false,
    )?;
    let mut bytes = Vec::new();
    file.take(limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| io(path, error))?;
    Ok(bytes)
}

/// Refuses files on platforms without the supported ownership model.
#[cfg(not(unix))]
pub fn read(_path: &Path, _limit: usize) -> Result<Vec<u8>, PrivateFileError> {
    Err(PrivateFileError::Unsupported)
}

/// Flushes the containing directory after publishing a new private file.
#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> Result<(), PrivateFileError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| io(path, error))
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_path: &Path) -> Result<(), PrivateFileError> {
    Err(PrivateFileError::Unsupported)
}

/// Atomically publishes a new owner-only file, flushing its bytes and parent directory.
/// Never overwrites existing state.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), PrivateFileError> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| PrivateFileError::Protection(path.to_owned()))?;
    directory(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|error| io(path, error))?;
    file.write_all(bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| io(path, error))?;
    file.persist_noclobber(path)
        .map_err(|error| io(path, error.error))?;
    sync_directory(parent)
}
