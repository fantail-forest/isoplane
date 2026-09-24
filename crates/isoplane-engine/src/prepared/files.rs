use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::native::NativeCopy;
use super::{
    Manifest, PreparedError, PreparedTemplate, Result, MANIFEST, PAYLOAD_NAMES,
};

/// One payload in the fixed prepared-artifact schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PreparedPayload {
    /// Serialized native module.
    Native,
    /// Sparse baseline index and payload.
    Baseline,
    /// Explicit initial state.
    State,
}

impl PreparedPayload {
    /// Canonical file name inside a published prepared-artifact directory.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Native => super::NATIVE,
            Self::Baseline => super::BASELINE,
            Self::State => super::STATE,
        }
    }

    pub(super) fn index(self) -> usize {
        match self {
            Self::Native => 0,
            Self::Baseline => 1,
            Self::State => 2,
        }
    }
}

/// A bounded advisory read for one fixed payload; offsets and lengths are bytes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ReadAhead {
    /// Index in the native-code, baseline, state payload order.
    pub payload: PreparedPayload,
    /// Byte offset from the beginning of the payload.
    pub offset_bytes: u64,
    /// Number of bytes to advise.
    pub length_bytes: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct FileRecord {
    pub(super) bytes: u64,
    pub(super) hash: [u8; 32],
}

impl FileRecord {
    pub(super) fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.len() as u64,
            hash: *blake3::hash(bytes).as_bytes(),
        }
    }
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> PreparedError {
    PreparedError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

impl PreparedTemplate {
    /// Atomically publishes a new directory of read-only payloads; existing
    /// versions are never overwritten.
    pub fn publish(&self, directory: &Path) -> Result<()> {
        let parent = directory.parent().ok_or(PreparedError::Invalid(
            "publication directory has no parent",
        ))?;
        std::fs::create_dir_all(parent).map_err(|e| io("create parent", parent, e))?;
        if directory.exists() {
            return Err(PreparedError::Invalid(
                "publication destination already exists",
            ));
        }
        let temporary =
            tempfile::tempdir_in(parent).map_err(|e| io("create staging directory", parent, e))?;
        for (index, name) in PAYLOAD_NAMES.iter().enumerate() {
            let path = temporary.path().join(name);
            write_new(&path, &self.payloads[index])?;
            let mut permissions = std::fs::metadata(&path)
                .map_err(|e| io("stat published payload", &path, e))?
                .permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&path, permissions)
                .map_err(|e| io("make published payload read-only", &path, e))?;
        }
        let metadata = postcard::to_allocvec(&self.manifest)?;
        write_new(&temporary.path().join(MANIFEST), &metadata)?;
        sync_directory(temporary.path())
            .map_err(|e| io("sync staging directory", temporary.path(), e))?;
        std::fs::rename(temporary.path(), directory)
            .map_err(|e| io("publish directory", directory, e))?;
        sync_directory(parent).map_err(|e| io("sync parent", parent, e))?;
        Ok(())
    }
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

        // Directory handles require backup semantics; FlushFileBuffers requires write access.
        options.write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    #[cfg(not(windows))]
    options.read(true);
    options.open(path)?.sync_all()
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::create_new(path).map_err(|e| io("create payload", path, e))?;
    file.write_all(bytes)
        .map_err(|e| io("write payload", path, e))?;
    file.sync_all().map_err(|e| io("sync payload", path, e))
}

#[tracing::instrument(name = "template_manifest_verify", skip_all)]
pub(super) fn read_manifest_receipt(
    directory: &Path,
) -> Result<(Manifest, [u8; 32])> {
    use sha2::Digest;
    let bytes = read_manifest_bytes(directory)?;
    Ok((
        postcard::from_bytes(&bytes)?,
        sha2::Sha256::digest(&bytes).into(),
    ))
}

/// The published local manifest, matching the exact digest that selection
/// recorded for the captured artifact.
#[tracing::instrument(name = "template_manifest_verify", skip_all)]
pub(super) fn read_manifest_by_digest(
    directory: &Path,
    manifest_sha256: [u8; 32],
) -> Result<Manifest> {
    use sha2::Digest;
    let bytes = read_manifest_bytes(directory)?;
    if <[u8; 32]>::from(sha2::Sha256::digest(&bytes)) != manifest_sha256 {
        return Err(PreparedError::Incompatible("catalog manifest digest"));
    }
    Ok(postcard::from_bytes(&bytes)?)
}

fn read_manifest_bytes(directory: &Path) -> Result<Vec<u8>> {
    let path = directory.join(MANIFEST);
    let file = File::open(&path).map_err(|e| io("open manifest", &path, e))?;
    // The schema has three fixed records and one engine identity, never arbitrary payload bytes.
    if file
        .metadata()
        .map_err(|e| io("stat manifest", &path, e))?
        .len()
        > 65536
    {
        return Err(PreparedError::Invalid("manifest exceeds schema bound"));
    }
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|e| io("read manifest", &path, e))?;
    if bytes.is_empty() || bytes.len() > 65536 {
        return Err(PreparedError::Invalid("manifest length"));
    }
    Ok(bytes)
}

/// A mapped template's payloads, verified against its manifest: the
/// native code copied into memory, the baseline and state as files to map.
/// The native copy is absent when the engine already holds the module the
/// manifest records, so it is neither read nor verified again.
pub(super) struct Verified {
    pub native: Option<NativeCopy>,
    pub baseline: File,
    pub state: File,
}

impl Verified {
    /// The payload files still read from storage, in `PAYLOAD_NAMES` order.
    pub fn each(&self) -> [Option<&File>; 3] {
        [None, Some(&self.baseline), Some(&self.state)]
    }
}

/// Opens and verifies the payloads, copying the native one only when `native`.
pub(super) fn open_verified(
    directory: &Path,
    manifest: &Manifest,
    native: bool,
) -> Result<Verified> {
    // The payloads verify concurrently, each hashed across cores, so a cold
    // load waits on the largest payload's read rather than the sum of the
    // single-threaded hashes.
    // Each payload's span nests under the caller's load, as it did on one thread.
    let parent = tracing::Span::current();
    std::thread::scope(|scope| {
        let native = native.then(|| {
            spawn_under(scope, &parent, || {
                let mut copy =
                    NativeCopy::new().map_err(|e| io("copy native payload", directory, e))?;
                open_verified_payload(directory, manifest, 0, &mut copy)?;
                Ok(copy)
            })
        });
        let file = |index| {
            spawn_under(scope, &parent, move || {
                open_verified_payload(directory, manifest, index, &mut std::io::sink())
            })
        };
        let baseline = file(1);
        let state = file(2);
        Ok(Verified {
            native: native.map(join).transpose()?,
            baseline: join(baseline)?,
            state: join(state)?,
        })
    })
}

fn spawn_under<'scope, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    parent: &tracing::Span,
    verify: impl FnOnce() -> Result<T> + Send + 'scope,
) -> std::thread::ScopedJoinHandle<'scope, Result<T>> {
    let parent = parent.clone();
    scope.spawn(move || parent.in_scope(verify))
}

fn join<T>(handle: std::thread::ScopedJoinHandle<'_, Result<T>>) -> Result<T> {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// A few hashing threads: more costs more in coordination than it saves on a
/// payload read in large blocks.
fn hash_pool() -> Result<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    if let Some(pool) = POOL.get() {
        return Ok(pool);
    }
    let threads = std::thread::available_parallelism().map_or(1, |cores| cores.get().min(8));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|index| format!("payload-hash-{index}"))
        .build()
        .map_err(|error| PreparedError::Engine(error.into()))?;
    Ok(POOL.get_or_init(|| pool))
}

/// Verifies one payload against the manifest, writing each block it hashes
/// to `copy`.
fn open_verified_payload(
    directory: &Path,
    manifest: &Manifest,
    index: usize,
    copy: &mut impl Write,
) -> Result<File> {
    const READ_BYTES: usize = 8 << 20;
    let _payload = tracing::info_span!(
        "template_payload_verify",
        payload = PAYLOAD_NAMES[index],
        bytes = manifest.files[index].bytes
    )
    .entered();
    let path = directory.join(PAYLOAD_NAMES[index]);
    let mut file = File::open(&path).map_err(|e| io("open payload", &path, e))?;
    let record = &manifest.files[index];
    if file
        .metadata()
        .map_err(|e| io("stat immutable payload", &path, e))?
        .len()
        != record.bytes
    {
        return Err(PreparedError::Invalid(
            "immutable payload differs from manifest",
        ));
    }
    let mut hash = blake3::Hasher::new();
    let mut buffer = vec![0; READ_BYTES];
    let mut bytes = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| io("verify payload", &path, e))?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        if bytes > record.bytes {
            return Err(PreparedError::Invalid(
                "payload differs from manifest",
            ));
        }
        hash_pool()?.install(|| hash.update_rayon(&buffer[..read]));
        copy.write_all(&buffer[..read])
            .map_err(|e| io("copy verified payload", &path, e))?;
    }
    if bytes != record.bytes || hash.finalize().as_bytes() != &record.hash {
        return Err(PreparedError::Invalid(
            "payload differs from manifest",
        ));
    }
    file.rewind()
        .map_err(|e| io("rewind verified payload", &path, e))?;
    Ok(file)
}

#[tracing::instrument(name = "template_read_ahead", skip_all, fields(ranges = ranges.len()))]
/// Asks the kernel to read `ranges` ahead. A range in a payload the engine
/// holds in memory is validated but not read.
pub(super) fn advise(files: &Verified, manifest: &Manifest, ranges: &[ReadAhead]) -> Result<()> {
    let mut ordered: Vec<_> = ranges.iter().collect();
    ordered.sort_by_key(|range| (range.payload, range.offset_bytes));
    let mut previous = None;
    for range in &ordered {
        let record = manifest
            .files
            .get(range.payload.index())
            .ok_or(PreparedError::Invalid("read-ahead payload index"))?;
        let end = range
            .offset_bytes
            .checked_add(range.length_bytes)
            .ok_or(PreparedError::Invalid("read-ahead overflow"))?;
        if range.length_bytes == 0
            || end > record.bytes
            || previous.is_some_and(|(payload, previous_end)| {
                payload == range.payload && range.offset_bytes < previous_end
            })
        {
            return Err(PreparedError::Invalid(
                "read-ahead is empty, overlapping or exceeds payload",
            ));
        }
        previous = Some((range.payload, end));
    }
    for range in ordered {
        if let Some(file) = files.each()[range.payload.index()] {
            advise_file(file, range)?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn advise_file(file: &File, range: &ReadAhead) -> Result<()> {
    use std::os::fd::AsRawFd;
    let offset = range
        .offset_bytes
        .try_into()
        .map_err(|_| PreparedError::Invalid("read-ahead offset exceeds platform limit"))?;
    let length = range
        .length_bytes
        .try_into()
        .map_err(|_| PreparedError::Invalid("read-ahead length exceeds platform limit"))?;
    // SAFETY: the descriptor is retained and the range was checked against its recorded length.
    let result =
        unsafe { libc::posix_fadvise(file.as_raw_fd(), offset, length, libc::POSIX_FADV_WILLNEED) };
    if result != 0 {
        return Err(io(
            "advise immutable payload",
            Path::new(PAYLOAD_NAMES[range.payload.index()]),
            std::io::Error::from_raw_os_error(result),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn advise_file(_file: &File, _range: &ReadAhead) -> Result<()> {
    Err(PreparedError::Invalid(
        "read-ahead is unsupported on this platform",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn fixture() -> (
        tempfile::TempDir,
        super::super::PreparedIdentity,
        crate::IsoplaneEngine,
    ) {
        let directory = tempfile::tempdir().expect("fixture directory");
        let engine = crate::IsoplaneEngine::new(crate::EngineConfig {
            max_instances: 4,
            ..Default::default()
        })
        .expect("engine");
        let wasm = wat::parse_str(
            "(module (memory (export \"memory\") 1) (data (i32.const 7) \"held inode\"))",
        )
        .expect("wasm");
        let identity = super::super::PreparedIdentity {
            input_hash: [7; 32],
            runtime_profile: [8; 32],
            initialization_hash: [3; 32],
        };
        engine
            .prepare_template(&wasm, identity)
            .expect("prepare")
            .publish(&directory.path().join("version"))
            .expect("mapped publication");
        (directory, identity, engine)
    }

    #[test]
    fn the_native_code_loaded_is_the_code_verified_after_path_replacement() {
        let (directory, _, engine) = fixture();
        let path = directory.path().join("version");
        let manifest = read_manifest_receipt(&path).expect("manifest").0;
        let files = open_verified(&path, &manifest, true).expect("verify held files");
        std::fs::rename(path.join(super::super::NATIVE), path.join("old-native"))
            .expect("replace path");
        std::fs::write(
            path.join(super::super::NATIVE),
            b"replacement is not native code",
        )
        .expect("replacement");
        let native = files.native.expect("the native payload was requested");
        // SAFETY: the fixture locally compiled these bytes, and verification checked the private copy.
        let module =
            unsafe { native.deserialize(&engine.inner.wasmtime) }.expect("load the verified copy");
        assert!(
            module.get_export("memory").is_some(),
            "the verified native code must survive path replacement"
        );
        assert!(
            open_verified(&path, &manifest, true).is_err(),
            "a later load must reject the replacement"
        );
    }

    #[test]
    fn mapped_publication_marks_payloads_read_only() {
        let (directory, _, _) = fixture();
        for name in PAYLOAD_NAMES {
            let path = directory.path().join("version").join(name);
            assert!(
                std::fs::metadata(path)
                    .expect("metadata")
                    .permissions()
                    .readonly(),
                "mapped {name} must discourage accidental writes"
            );
        }
    }

    #[test]
    fn mapping_rejects_same_length_payload_tampering() {
        let (directory, _, _) = fixture();
        let path = directory.path().join("version");
        let manifest = read_manifest_receipt(&path).expect("manifest").0;
        for name in PAYLOAD_NAMES {
            let payload = path.join(name);
            let original = std::fs::read(&payload).expect("read payload");
            let mut changed = original.clone();
            changed[0] ^= 1;
            std::fs::remove_file(&payload).expect("remove payload");
            std::fs::write(&payload, changed).expect("replace payload");
            assert!(
                matches!(
                    open_verified(&path, &manifest, true),
                    Err(PreparedError::Invalid(_))
                ),
                "mapping must reject same-length tampering in {name}"
            );
            std::fs::write(&payload, original).expect("restore payload");
        }
    }

    #[test]
    fn read_ahead_rejects_unbounded_or_overlapping_ranges() {
        let (directory, _, _) = fixture();
        let path = directory.path().join("version");
        let manifest = read_manifest_receipt(&path).expect("manifest").0;
        let files = open_verified(&path, &manifest, true).expect("files");
        for ranges in [
            vec![ReadAhead {
                payload: PreparedPayload::Native,
                offset_bytes: 0,
                length_bytes: 0,
            }],
            vec![ReadAhead {
                payload: PreparedPayload::Native,
                offset_bytes: u64::MAX,
                length_bytes: 1,
            }],
            vec![ReadAhead {
                payload: PreparedPayload::Native,
                offset_bytes: 0,
                length_bytes: manifest.files[0].bytes + 1,
            }],
            vec![
                ReadAhead {
                    payload: PreparedPayload::Native,
                    offset_bytes: 0,
                    length_bytes: 2,
                },
                ReadAhead {
                    payload: PreparedPayload::Native,
                    offset_bytes: 1,
                    length_bytes: 2,
                },
            ],
        ] {
            assert!(
                advise(&files, &manifest, &ranges).is_err(),
                "malformed read-ahead must fail before kernel advice"
            );
        }
        advise(
            &files,
            &manifest,
            &[ReadAhead {
                payload: PreparedPayload::Native,
                offset_bytes: 0,
                length_bytes: manifest.files[0].bytes,
            }],
        )
        .expect("whole-file bounded hint");
    }
}
