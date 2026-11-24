//! Preparation, inspection and clearing of the per-user runtime cache local
//! sessions fill.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::Digest;

use crate::error::Error;
use crate::interop::{NativeManifest, NativeText};

/// The compiled runtimes kept at `directory`, without creating it.
pub(crate) fn inventory(directory: &[u8]) -> Result<Inventory, Error> {
    Inventory::of(&path(directory)?)
}

/// The compiled runtimes a cache holds.
pub(crate) struct Inventory {
    /// The most the cache keeps before it removes the runtimes used least recently.
    budget_bytes: u64,
    entries: Vec<CompiledRuntime>,
}

/// One compiled runtime in the cache.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompiledRuntime {
    pub bytes: u64,
    pub last_used_unix_ms: u64,
}

/// What clearing a cache did.
pub(crate) struct Cleared {
    /// The cache as clearing left it.
    remaining: Inventory,
    removed_bytes: u64,
    /// Why each remaining runtime could not be removed.
    failures: Vec<String>,
}

/// The compiled runtimes a cache holds, released with
/// `isoplane_runtime_cache_inventory_free`.
#[repr(C)]
pub struct NativeCacheInventory {
    /// The most the cache keeps before it removes the runtimes used least recently.
    pub budget_bytes: u64,
    pub entries: *mut CompiledRuntime,
    pub entries_length: usize,
}

/// What clearing a cache did, released with `isoplane_runtime_cache_cleared_free`.
#[repr(C)]
pub struct NativeCacheCleared {
    /// The cache as clearing left it.
    pub remaining: NativeCacheInventory,
    pub removed_bytes: u64,
    /// Why each remaining runtime could not be removed.
    pub failures: *mut NativeText,
    pub failures_length: usize,
}

/// Removes every compiled runtime kept at `directory`, and reports what was
/// removed, what could not be and what remains.
pub(crate) fn clear(directory: &[u8]) -> Result<Cleared, Error> {
    let directory = path(directory)?;
    let before = Inventory::of(&directory)?;
    let failures = match directory.exists() {
        true => cache(&directory)?.clear()?,
        false => Vec::new(),
    };
    let remaining = Inventory::of(&directory)?;
    Ok(Cleared {
        removed_bytes: before.bytes().saturating_sub(remaining.bytes()),
        remaining,
        failures: failures.iter().map(ToString::to_string).collect(),
    })
}

/// What a shared runtime is compiled for: the settings a session of an
/// application package on it would compile it with.
struct Preparation {
    manifest: isoplane_abi::config::AppManifest,
    runtime_library: PathBuf,
    runtime_cache_directory: PathBuf,
    machine_roots: Option<PathBuf>,
    /// The runtime's `runtime.json`, which names its SDK and the runtime module's digest.
    runtime_identity: String,
}

/// What a shared runtime is compiled for, as the managed caller lends it for the call.
#[repr(C)]
pub struct NativeRuntimeOptions {
    pub manifest: NativeManifest,
    /// The native library, whose identity the compiled runtime depends on.
    pub runtime_library: NativeText,
    /// The absolute directory of the cache to compile into.
    pub runtime_cache_directory: NativeText,
    /// The machine's trusted roots exported as PEM, or empty to trust the shipped roots alone.
    pub machine_roots: NativeText,
    /// The runtime's `runtime.json`, which names its SDK and the runtime module's digest.
    pub runtime_identity: NativeText,
}

impl NativeRuntimeOptions {
    /// # Safety
    /// Every text is readable for its length for this call.
    unsafe fn read(&self) -> Result<Preparation, Error> {
        // SAFETY: forwarded from the caller.
        unsafe {
            Ok(Preparation {
                manifest: self.manifest.read()?,
                runtime_library: self.runtime_library.read()?.into(),
                runtime_cache_directory: self.runtime_cache_directory.read()?.into(),
                machine_roots: self.machine_roots.optional()?.map(PathBuf::from),
                runtime_identity: self.runtime_identity.read()?.into(),
            })
        }
    }
}

/// Compiles the shared runtime `wasm` and its debug build `debug_wasm` into
/// the runtime cache, and reports whether each had to be compiled.
///
/// # Safety
/// As for `NativeRuntimeOptions::read`.
pub(crate) unsafe fn prepare(
    wasm: &[u8],
    debug_wasm: &[u8],
    options: &NativeRuntimeOptions,
) -> Result<crate::local::RuntimePreparation, Error> {
    // SAFETY: forwarded from the caller.
    let options = unsafe { options.read()? };
    prepare_runtime(wasm, debug_wasm, options).map_err(Error::RuntimePreparation)
}

fn prepare_runtime(
    wasm: &[u8],
    debug_wasm: &[u8],
    options: Preparation,
) -> anyhow::Result<crate::local::RuntimePreparation> {
    anyhow::ensure!(
        options.runtime_cache_directory.is_absolute(),
        "the runtime cache directory must be absolute"
    );
    let identity =
        isoplane_prepare::package::RuntimeIdentity::from_json(options.runtime_identity.as_bytes())?;
    anyhow::ensure!(
        <[u8; 32]>::from(sha2::Sha256::digest(wasm)) == identity.module_sha256,
        "the runtime module is not the one its runtime.json names"
    );
    let inputs = crate::local::Inputs {
        manifest: options.manifest,
        // Neither is read: a runtime has no application state or files of its own.
        state_directory: options.runtime_cache_directory.clone(),
        compilation_directory: options.runtime_cache_directory.clone(),
        runtime_library: options.runtime_library,
        runtime_cache_directory: options.runtime_cache_directory,
        platform_files: isoplane_engine::PlatformFiles::new(vec![crate::session::trust_file(
            options.machine_roots.as_deref(),
        )?])?,
        application_files: isoplane_engine::PlatformFiles::default(),
        runtime_profile: isoplane_prepare::RuntimeProfile::SharedRuntime {
            sdk_version: identity.sdk_version,
        },
    };
    crate::local::prepare_runtime(&inputs, wasm, debug_wasm)
}

fn path(directory: &[u8]) -> Result<PathBuf, Error> {
    std::str::from_utf8(directory)
        .ok()
        .map(PathBuf::from)
        .filter(|directory| directory.is_absolute())
        .ok_or(Error::CacheDirectory)
}

fn cache(directory: &Path) -> Result<isoplane_cache::Cache, Error> {
    Ok(isoplane_cache::Cache::new(
        directory.into(),
        isoplane_cache::RUNTIME_CACHE_BYTES,
    )?)
}

impl Inventory {
    /// The cache at `directory`, which is empty when the directory does not exist.
    fn of(directory: &Path) -> Result<Self, Error> {
        let entries = match directory.exists() {
            true => cache(directory)?.entries()?,
            false => Vec::new(),
        };
        Ok(Self {
            budget_bytes: isoplane_cache::RUNTIME_CACHE_BYTES.0,
            entries: entries
                .iter()
                .map(|entry| CompiledRuntime {
                    bytes: entry.bytes,
                    last_used_unix_ms: entry
                        .last_used
                        .duration_since(UNIX_EPOCH)
                        .map_or(0, |since| since.as_millis() as u64),
                })
                .collect(),
        })
    }

    fn bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).sum()
    }

    /// The inventory as the managed caller receives it.
    pub(crate) fn native(self) -> NativeCacheInventory {
        let (entries, entries_length) = crate::interop::owned(self.entries);
        NativeCacheInventory {
            budget_bytes: self.budget_bytes,
            entries,
            entries_length,
        }
    }
}

impl NativeCacheInventory {
    /// # Safety
    /// `self` came from `Inventory::native` and is released once.
    pub(crate) unsafe fn release(self) {
        // SAFETY: forwarded from the caller.
        drop(unsafe { crate::interop::release(self.entries, self.entries_length) });
    }
}

impl Cleared {
    /// The report as the managed caller receives it.
    pub(crate) fn native(self) -> NativeCacheCleared {
        let (failures, failures_length) =
            crate::interop::owned(self.failures.into_iter().map(NativeText::owned).collect());
        NativeCacheCleared {
            remaining: self.remaining.native(),
            removed_bytes: self.removed_bytes,
            failures,
            failures_length,
        }
    }
}

impl NativeCacheCleared {
    /// # Safety
    /// `self` came from `Cleared::native` and is released once.
    pub(crate) unsafe fn release(self) {
        // SAFETY: forwarded from the caller.
        unsafe {
            self.remaining.release();
            for failure in crate::interop::release(self.failures, self.failures_length) {
                failure.release();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(root: &tempfile::TempDir) -> Vec<u8> {
        root.path()
            .join("runtimes")
            .to_str()
            .expect("a temporary path is UTF-8")
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn an_inventory_of_a_cache_no_session_has_filled_is_empty_and_creates_nothing() {
        let root = tempfile::tempdir().expect("directory");
        let report = inventory(&directory(&root)).expect("inventory");
        assert!(
            report.entries.is_empty(),
            "a cache that does not exist holds nothing"
        );
        assert!(
            !root.path().join("runtimes").exists(),
            "inspecting a cache must not create it"
        );
    }

    #[test]
    fn clearing_reports_the_bytes_it_removed_and_leaves_the_cache_empty() {
        let root = tempfile::tempdir().expect("directory");
        let storage = cache(&root.path().join("runtimes")).expect("cache");
        let lease = storage
            .lease(isoplane_cache::CacheKey([7; 32]))
            .expect("lease");
        std::fs::write(lease.directory().join("runtime.cwasm"), [0; 4096]).expect("artifact");
        drop(lease);
        let before = inventory(&directory(&root)).expect("inventory");
        assert_eq!(
            before
                .entries
                .iter()
                .map(|entry| entry.bytes)
                .collect::<Vec<_>>(),
            [4096],
            "an inventory must size each compiled runtime"
        );
        let cleared = clear(&directory(&root)).expect("clear");
        assert_eq!(
            (
                cleared.removed_bytes,
                cleared.remaining.entries.len(),
                cleared.failures.len()
            ),
            (4096, 0, 0),
            "clearing must remove every compiled runtime and say how much it freed"
        );
    }

    #[test]
    fn a_relative_cache_directory_is_refused() {
        assert!(
            matches!(inventory(b"relative/runtimes"), Err(Error::CacheDirectory)),
            "a cache is named by an absolute path, never one resolved against a working directory"
        );
    }
}
