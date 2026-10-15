//! Owned local storage for same-process suspension. Reservations never enter
//! the expiring or regional spool and remain allocated until their owner drops.

use std::fs::File;
use std::io::{self, IoSlice, IoSliceMut, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs4::FileExt;

/// Bytes of local snapshot capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotBytes(pub u64);

/// A local snapshot operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// The filesystem refused an operation.
    #[error("{operation} snapshot storage at {path}: {source}")]
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Storage directory.
        path: PathBuf,
        /// Filesystem failure, including exhausted capacity or quota.
        #[source]
        source: io::Error,
    },
    /// The filesystem did not back the requested capacity with allocated blocks.
    #[error("snapshot storage at {path} could only reserve {allocated_bytes} of {requested_bytes} bytes", allocated_bytes = .allocated.0, requested_bytes = .requested.0)]
    Unreserved {
        /// Storage directory.
        path: PathBuf,
        /// Required capacity.
        requested: SnapshotBytes,
        /// Capacity actually allocated.
        allocated: SnapshotBytes,
    },
    /// A snapshot would exceed its owned reservation.
    #[error("snapshot requires {required:?}, reservation holds {reserved:?}")]
    TooLarge {
        /// Encoded snapshot length.
        required: SnapshotBytes,
        /// Reservation capacity.
        reserved: SnapshotBytes,
    },
    /// No complete snapshot has been saved, or a write failed.
    #[error("snapshot reservation has no complete saved snapshot")]
    Empty,
    /// A read reached past the end of the saved snapshot.
    #[error("snapshot read ends at byte {end}, past the {saved} saved bytes")]
    BeyondSnapshot {
        /// Offset one past the last byte requested.
        end: u64,
        /// Length of the saved snapshot.
        saved: u64,
    },
}

impl SnapshotError {
    /// Whether releasing other reservations can address this failure.
    pub fn is_capacity_exhausted(&self) -> bool {
        matches!(self, Self::Io { source, .. } if matches!(source.kind(), io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded))
    }
}

/// Reserves capacity before an instance is admitted.
pub trait SnapshotStorage: Send + Sync {
    /// Own filesystem capacity until the returned reservation is dropped.
    /// Failure admits no reservation and retains no allocated file.
    fn reserve(
        &self,
        capacity: SnapshotBytes,
    ) -> Result<Box<dyn SnapshotReservation>, SnapshotError>;
}

/// Storage owned by one instance, retained through every suspend/wake cycle.
pub trait SnapshotReservation: Send {
    /// Capacity available for an encoded snapshot.
    fn reserved_bytes(&self) -> SnapshotBytes;

    /// Replace the snapshot within the reservation with `parts`, in order,
    /// without temporary files or flushing to storage. An I/O failure
    /// invalidates the saved snapshot; its resident source must remain owned
    /// until this succeeds. An oversized save preserves prior data.
    fn save(&mut self, parts: &[IoSlice<'_>]) -> Result<(), SnapshotError>;

    /// Fill `parts`, in order, from the last complete snapshot starting at
    /// `offset`, without releasing storage capacity.
    fn read_exact_at(
        &mut self,
        offset: u64,
        parts: &mut [IoSliceMut<'_>],
    ) -> Result<(), SnapshotError>;
}

/// Private, preallocated files, reclaimed on drop or process termination.
/// Filesystems that cannot allocate physical capacity are rejected.
pub struct LocalSnapshotStorage {
    root: PathBuf,
}

impl LocalSnapshotStorage {
    /// Create the local storage directory, failing if it cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, SnapshotError> {
        let storage = Self { root: root.into() };
        std::fs::create_dir_all(&storage.root)
            .map_err(|source| storage.io_error("create directory for", source))?;
        Ok(storage)
    }

    fn io_error(&self, operation: &'static str, source: io::Error) -> SnapshotError {
        SnapshotError::Io {
            operation,
            path: self.root.clone(),
            source,
        }
    }

    fn reserve_with(
        &self,
        capacity: SnapshotBytes,
        allocate: impl FnOnce(&File, u64) -> io::Result<()>,
    ) -> Result<LocalSnapshotReservation, SnapshotError> {
        let file = snapshot_file(&self.root)
            .map_err(|source| self.io_error("create anonymous file for", source))?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|source| self.io_error("restrict access to", source))?;
        }
        allocate(&file, capacity.0)
            .map_err(|source| self.io_error("reserve capacity for", source))?;
        let allocated = file
            .allocated_size()
            .map_err(|source| self.io_error("verify reserved capacity for", source))?;
        if allocated < capacity.0 {
            return Err(SnapshotError::Unreserved {
                path: self.root.clone(),
                requested: capacity,
                allocated: SnapshotBytes(allocated),
            });
        }
        Ok(LocalSnapshotReservation {
            file,
            root: self.root.clone(),
            capacity,
            saved_bytes: None,
        })
    }
}

impl SnapshotStorage for LocalSnapshotStorage {
    #[tracing::instrument(name = "snapshot_reserve", skip_all, fields(reserved_bytes = capacity.0))]
    fn reserve(
        &self,
        capacity: SnapshotBytes,
    ) -> Result<Box<dyn SnapshotReservation>, SnapshotError> {
        self.reserve_with(capacity, allocate_snapshot)
            .map(|reservation| Box::new(reservation) as Box<dyn SnapshotReservation>)
    }
}

#[cfg(not(target_os = "macos"))]
fn snapshot_file(root: &Path) -> io::Result<File> {
    tempfile::tempfile_in(root)
}

#[cfg(target_os = "macos")]
fn snapshot_file(root: &Path) -> io::Result<File> {
    let temporary = tempfile::NamedTempFile::new_in(root)?;
    let mut metadata_name = std::ffi::OsString::from("._");
    metadata_name.push(
        temporary.path().file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "snapshot has no file name")
        })?,
    );
    let metadata = temporary.path().with_file_name(metadata_name);
    let (file, path) = temporary.into_parts();
    path.close()?;
    // macOS provenance can leave an AppleDouble file after unlinking on a share.
    match std::fs::remove_file(metadata) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(file)
}

fn allocate_snapshot(file: &File, bytes: u64) -> io::Result<()> {
    match preallocate_snapshot(file, bytes) {
        // Some shares accept preallocation but leave the file sparse. Writing
        // the capacity is a fallback; reserve_with still verifies the blocks.
        Ok(()) if file.allocated_size()? < bytes => materialize_snapshot(file, bytes),
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            materialize_snapshot(file, bytes)
        }
        result => result,
    }
}

#[tracing::instrument(skip(file))]
fn materialize_snapshot(file: &File, bytes: u64) -> io::Result<()> {
    // Filesystems without preallocation still need real blocks, not sparse length.
    let mut writer = io::BufWriter::with_capacity(1024 * 1024, file);
    io::copy(&mut io::repeat(0).take(bytes), &mut writer)?;
    writer.flush()?;
    flush_snapshot(file)
}

#[cfg(not(target_os = "macos"))]
fn flush_snapshot(file: &File) -> io::Result<()> {
    file.sync_data()
}

#[cfg(target_os = "macos")]
fn flush_snapshot(file: &File) -> io::Result<()> {
    // These anonymous snapshots only survive within this process. fsync is
    // sufficient; Rust's F_FULLFSYNC requires device guarantees absent on SMB.
    nix::unistd::fsync(file).map_err(Into::into)
}

#[cfg(not(target_os = "macos"))]
#[tracing::instrument(skip(file))]
fn preallocate_snapshot(file: &File, bytes: u64) -> io::Result<()> {
    file.allocate(bytes)
}

#[cfg(target_os = "macos")]
#[tracing::instrument(skip(file))]
fn preallocate_snapshot(file: &File, bytes: u64) -> io::Result<()> {
    use nix::{fcntl, libc};

    // F_ALLOCATECONTIG can succeed with a partial allocation. Admission needs
    // all blocks reserved, regardless of whether they are contiguous.
    let mut allocation = libc::fstore_t {
        fst_flags: libc::F_ALLOCATEALL,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: bytes.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot exceeds file capacity",
            )
        })?,
        fst_bytesalloc: 0,
    };
    fcntl::fcntl(file, fcntl::FcntlArg::F_PREALLOCATE(&mut allocation)).map_err(
        |error| match error {
            nix::errno::Errno::ENOTSUP => io::Error::new(io::ErrorKind::Unsupported, error),
            error => error.into(),
        },
    )?;
    file.set_len(bytes)
}

struct LocalSnapshotReservation {
    file: File,
    root: PathBuf,
    capacity: SnapshotBytes,
    saved_bytes: Option<u64>,
}

impl LocalSnapshotReservation {
    fn io_error(&self, operation: &'static str, source: io::Error) -> SnapshotError {
        SnapshotError::Io {
            operation,
            path: self.root.clone(),
            source,
        }
    }
}

impl SnapshotReservation for LocalSnapshotReservation {
    fn reserved_bytes(&self) -> SnapshotBytes {
        self.capacity
    }

    fn save(&mut self, parts: &[IoSlice<'_>]) -> Result<(), SnapshotError> {
        let bytes = parts.iter().map(|part| part.len() as u64).sum::<u64>();
        let _span = tracing::info_span!("snapshot_save", bytes).entered();
        if bytes > self.capacity.0 {
            return Err(SnapshotError::TooLarge {
                required: SnapshotBytes(bytes),
                reserved: self.capacity,
            });
        }
        self.saved_bytes = None;
        self.file
            .rewind()
            .and_then(|()| write_all_vectored(&mut self.file, parts))
            .map_err(|source| self.io_error("save", source))?;
        self.saved_bytes = Some(bytes);
        Ok(())
    }

    fn read_exact_at(
        &mut self,
        offset: u64,
        parts: &mut [IoSliceMut<'_>],
    ) -> Result<(), SnapshotError> {
        let saved = self.saved_bytes.ok_or(SnapshotError::Empty)?;
        let bytes = parts.iter().map(|part| part.len() as u64).sum::<u64>();
        let _span = tracing::info_span!("snapshot_read", bytes).entered();
        let end = offset.saturating_add(bytes);
        if end > saved {
            return Err(SnapshotError::BeyondSnapshot { end, saved });
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| read_exact_vectored(&mut self.file, parts))
            .map_err(|source| self.io_error("load", source))
    }
}

fn write_all_vectored(file: &mut File, parts: &[IoSlice<'_>]) -> io::Result<()> {
    let mut parts = parts.to_vec();
    let mut rest = &mut parts[..];
    IoSlice::advance_slices(&mut rest, 0);
    while !rest.is_empty() {
        match file.write_vectored(rest) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => IoSlice::advance_slices(&mut rest, written),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_exact_vectored(file: &mut File, mut rest: &mut [IoSliceMut<'_>]) -> io::Result<()> {
    IoSliceMut::advance_slices(&mut rest, 0);
    while !rest.is_empty() {
        match file.read_vectored(rest) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => IoSliceMut::advance_slices(&mut rest, read),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save(reservation: &mut dyn SnapshotReservation, bytes: &[u8]) -> Result<(), SnapshotError> {
        reservation.save(&[IoSlice::new(bytes)])
    }

    fn load(reservation: &mut LocalSnapshotReservation) -> Result<Vec<u8>, SnapshotError> {
        let mut bytes = vec![0; reservation.saved_bytes.unwrap_or(0) as usize];
        reservation.read_exact_at(0, &mut [IoSliceMut::new(&mut bytes)])?;
        Ok(bytes)
    }

    fn assert_reclaimed(root: &std::path::Path) {
        let retained = std::fs::read_dir(root)
            .expect("list released snapshots")
            .map(|entry| entry.expect("read snapshot entry").path())
            .collect::<Vec<_>>();
        assert!(
            retained.is_empty(),
            "closed snapshots retained files: {retained:?}"
        );
    }

    #[test]
    fn materialized_reservations_own_blocks_and_release_them_on_drop() {
        let root = tempfile::tempdir().expect("create snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let capacity = SnapshotBytes(2 * 1024 * 1024 + 1);
        let mut reservation = storage
            .reserve_with(capacity, materialize_snapshot)
            .expect("materialized capacity must pass physical block verification");
        assert_eq!(reservation.reserved_bytes(), capacity);
        save(&mut reservation, b"saved snapshot").expect("save snapshot");
        assert_eq!(
            load(&mut reservation).expect("load snapshot"),
            b"saved snapshot"
        );
        assert!(reservation.file.allocated_size().expect("allocated blocks") >= capacity.0);
        drop(reservation);
        assert_reclaimed(root.path());
    }

    #[test]
    fn a_guest_sized_reservation_owns_all_blocks_before_saving() {
        let root = tempfile::tempdir().expect("create snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let capacity = SnapshotBytes(135_071_379);
        let reservation = storage
            .reserve_with(capacity, allocate_snapshot)
            .expect("reserve an unaligned guest-sized snapshot");
        assert!(
            reservation
                .file
                .allocated_size()
                .expect("inspect allocated blocks")
                >= capacity.0,
            "guest admission must own the entire reservation, not a partial contiguous extent"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_termination_reclaims_anonymous_snapshot_files() {
        use std::io::BufRead;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        use std::process::{Command, Stdio};

        const CHILD_ROOT: &str = "ISOPLANE_SNAPSHOT_RESERVATION_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let storage = LocalSnapshotStorage::new(PathBuf::from(root))
                .expect("open child snapshot storage");
            let reservation = storage
                .reserve_with(SnapshotBytes(8192), FileExt::allocate)
                .expect("reserve child snapshot blocks");
            println!("reservation-ready:{}", reservation.file.as_raw_fd());
            io::stdout().flush().expect("announce child reservation");
            let mut stop = [0];
            io::stdin().read_exact(&mut stop).expect("wait for parent");
            panic!("parent must kill the child while its reservation is owned");
        }

        let root = tempfile::tempdir().expect("create owned snapshot directory");
        let mut child = Command::new(std::env::current_exe().expect("locate test executable"))
            .args([
                "--exact",
                "snapshot::tests::process_termination_reclaims_anonymous_snapshot_files",
                "--nocapture",
            ])
            .env(CHILD_ROOT, root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start reservation owner process");
        let output = child.stdout.take().expect("take child stdout");
        let descriptor = io::BufReader::new(output).lines().find_map(|line| {
            let line = line.expect("read child reservation announcement");
            line.split_once("reservation-ready:")
                .map(|(_, descriptor)| descriptor.to_string())
        });
        let path = descriptor
            .map(|descriptor| PathBuf::from(format!("/proc/{}/fd/{descriptor}", child.id())));
        let metadata = path.as_ref().map(std::fs::metadata);
        child.kill().expect("kill owner with live reservation");
        child.wait().expect("reap reservation owner");
        let path = path.expect("child must announce a live reservation");
        let metadata = metadata
            .expect("reservation descriptor must be known")
            .expect("inspect child-owned reserved file");
        assert!(
            metadata.blocks() * 512 >= 8192,
            "another process must observe the admitted physical reservation"
        );
        assert_eq!(
            metadata.nlink(),
            0,
            "the reservation must have no directory entry retaining blocks after death"
        );
        assert!(
            !path.exists(),
            "process termination must close the last descriptor holding the reserved inode"
        );
        assert_eq!(
            std::fs::read_dir(root.path())
                .expect("list snapshots")
                .count(),
            0,
            "process termination must leave no orphaned snapshot file"
        );
    }

    #[test]
    fn a_reservation_survives_repeated_saves_and_loads_without_releasing_blocks() {
        let root = tempfile::tempdir().expect("create owned snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let capacity = SnapshotBytes(8192);
        let mut reservation = storage
            .reserve_with(capacity, allocate_snapshot)
            .expect("reserve snapshot blocks");
        let allocated_bytes = reservation.file.allocated_size().expect("read allocation");
        assert!(
            allocated_bytes >= capacity.0,
            "admission must own physical blocks covering its maximum snapshot"
        );
        for bytes in [vec![1; 8192], vec![2; 1], vec![], vec![3; 8192]] {
            save(&mut reservation, &bytes).expect("save within reservation");
            assert_eq!(
                load(&mut reservation).expect("load snapshot"),
                bytes,
                "wake must restore exactly the latest snapshot without trailing old data"
            );
            assert_eq!(
                reservation.file.allocated_size().expect("read allocation"),
                allocated_bytes,
                "saving or waking must retain every reserved block"
            );
            assert_eq!(
                reservation.reserved_bytes(),
                capacity,
                "repeated suspension must retain its original admission capacity"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                reservation
                    .file
                    .metadata()
                    .expect("snapshot metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "NFS can retain an unlinked inode under a private .nfs entry"
            );
        }
        // Windows delete-on-close entries deny access through other handles.
        #[cfg(windows)]
        for entry in std::fs::read_dir(root.path()).expect("list snapshots") {
            let path = entry.expect("read snapshot entry").path();
            assert!(
                File::open(&path).is_err(),
                "reservation is readable at {path:?}"
            );
            assert!(
                std::fs::remove_file(&path).is_err(),
                "reservation can be evicted at {path:?}"
            );
        }
        drop(reservation);
        assert_reclaimed(root.path());
    }

    #[test]
    fn an_oversized_save_preserves_the_last_complete_snapshot() {
        let root = tempfile::tempdir().expect("create owned snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let mut reservation = storage
            .reserve_with(SnapshotBytes(8), allocate_snapshot)
            .expect("reserve snapshot");
        save(&mut reservation, b"old").expect("save original snapshot");
        assert!(
            matches!(
                save(&mut reservation, b"too large"),
                Err(SnapshotError::TooLarge { .. })
            ),
            "a snapshot must never write beyond the capacity admitted for its instance"
        );
        assert_eq!(
            load(&mut reservation).expect("read original snapshot"),
            b"old",
            "rejecting an oversized snapshot must preserve the complete previous snapshot"
        );
    }

    #[test]
    fn a_reservation_cannot_load_before_a_complete_save() {
        let root = tempfile::tempdir().expect("create owned snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let mut reservation = storage
            .reserve_with(SnapshotBytes(8), allocate_snapshot)
            .expect("reserve snapshot");
        assert!(
            matches!(load(&mut reservation), Err(SnapshotError::Empty)),
            "preallocated zeroes must never be returned as a saved snapshot"
        );
    }

    #[test]
    fn parts_are_saved_in_order_and_read_back_from_any_offset_into_several_buffers() {
        let root = tempfile::tempdir().expect("create owned snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let mut reservation = storage
            .reserve_with(SnapshotBytes(64), allocate_snapshot)
            .expect("reserve snapshot");
        reservation
            .save(&[
                IoSlice::new(b"head"),
                IoSlice::new(b""),
                IoSlice::new(b"-middle-"),
                IoSlice::new(b"tail"),
            ])
            .expect("save several parts");
        let (mut first, mut second) = ([0; 3], [0; 6]);
        reservation
            .read_exact_at(
                5,
                &mut [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)],
            )
            .expect("read a saved range into two buffers");
        assert_eq!(
            (&first[..], &second[..]),
            (&b"mid"[..], &b"dle-ta"[..]),
            "a read must continue across buffers from its offset"
        );
        assert!(
            matches!(
                reservation.read_exact_at(14, &mut [IoSliceMut::new(&mut [0; 3])]),
                Err(SnapshotError::BeyondSnapshot { end: 17, saved: 16 })
            ),
            "preallocated bytes past the saved snapshot must never be read as part of it"
        );
    }

    #[test]
    fn allocation_exhaustion_is_reported_without_leaving_a_file() {
        let root = tempfile::tempdir().expect("create owned snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let result = storage.reserve_with(SnapshotBytes(4096), |file, capacity| {
            allocate_snapshot(file, capacity)?;
            Err(io::Error::from(io::ErrorKind::StorageFull))
        });
        assert!(
            result.is_err_and(|error| error.is_capacity_exhausted()),
            "admission must receive a typed capacity failure after a failed allocation"
        );
        assert_reclaimed(root.path());
    }

    #[test]
    fn unallocated_capacity_is_rejected() {
        let root = tempfile::tempdir().expect("create snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let result = storage.reserve_with(SnapshotBytes(8192), |_, _| Ok(()));
        assert!(matches!(result, Err(SnapshotError::Unreserved { .. })));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sparse_file_does_not_satisfy_a_capacity_reservation() {
        let root = tempfile::tempdir().expect("create owned snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let result = storage.reserve_with(SnapshotBytes(8192), File::set_len);
        assert!(
            matches!(result, Err(SnapshotError::Unreserved { .. })),
            "logical file length must never count as physically reserved storage"
        );
    }

    #[test]
    fn a_failed_save_invalidates_the_previous_snapshot() {
        let root = tempfile::tempdir().expect("create owned snapshot directory");
        let storage = LocalSnapshotStorage::new(root.path()).expect("open snapshot storage");
        let mut reservation = storage
            .reserve_with(SnapshotBytes(4096), allocate_snapshot)
            .expect("reserve snapshot blocks");
        save(&mut reservation, b"old").expect("save original snapshot");
        let path = root.path().join("read-only");
        std::fs::write(&path, b"old").expect("prepare read-only file");
        reservation.file = File::open(path).expect("open file without write access");
        assert!(
            matches!(
                save(&mut reservation, b"new"),
                Err(SnapshotError::Io { .. })
            ),
            "a failed write must be returned to the owner retaining the resident instance"
        );
        assert!(
            matches!(load(&mut reservation), Err(SnapshotError::Empty)),
            "a failed overwrite must never leave stale snapshot metadata readable"
        );
    }
}
