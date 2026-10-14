//! Preparation, inspection and clearing of the per-user runtime cache local
//! sessions fill.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::Digest;

use crate::validation::Error;

/// Reports the compiled runtimes kept at `directory`, without creating it.
pub(crate) fn inventory(directory: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(report(&path(directory)?)?.to_string().into_bytes())
}

/// Removes every compiled runtime kept at `directory`, and reports what was
/// removed, what could not be and what remains.
pub(crate) fn clear(directory: &[u8]) -> Result<Vec<u8>, Error> {
    let directory = path(directory)?;
    let before = report(&directory)?;
    let failures = match directory.exists() {
        true => cache(&directory)?.clear()?,
        false => Vec::new(),
    };
    let mut after = report(&directory)?;
    after["removed_bytes"] = bytes(&before).saturating_sub(bytes(&after)).into();
    after["failures"] = failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .into();
    Ok(after.to_string().into_bytes())
}

/// What a shared runtime is compiled for: the settings a session of an
/// application package on it would compile it with.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Preparation {
    manifest: isoplane_abi::config::AppManifest,
    runtime_library: PathBuf,
    runtime_cache_directory: PathBuf,
    machine_roots: Option<PathBuf>,
    /// The runtime's `runtime.json`, which names its SDK and the runtime module's digest.
    runtime_identity: String,
}

/// Compiles the shared runtime `wasm` and its debug build `debug_wasm` into
/// the runtime cache, and reports whether each had to be compiled.
pub(crate) fn prepare(wasm: &[u8], debug_wasm: &[u8], options: &[u8]) -> Result<Vec<u8>, Error> {
    let options: Preparation = serde_json::from_slice(options)?;
    let prepared = prepare_runtime(wasm, debug_wasm, options).map_err(Error::RuntimePreparation)?;
    Ok(serde_json::json!({
        "baseline_compiled": prepared.baseline_compiled,
        "debug_compiled": prepared.debug_compiled,
    })
    .to_string()
    .into_bytes())
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
        init_sandbox: crate::session::RUNTIME_SANDBOX,
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

fn report(directory: &Path) -> Result<serde_json::Value, Error> {
    let entries = match directory.exists() {
        true => cache(directory)?.entries()?,
        false => Vec::new(),
    };
    Ok(serde_json::json!({
        "budget_bytes": isoplane_cache::RUNTIME_CACHE_BYTES.0,
        "entries": entries.iter().map(|entry| serde_json::json!({
            "bytes": entry.bytes,
            "last_used_unix_ms": entry.last_used.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64),
        })).collect::<Vec<_>>(),
    }))
}

fn bytes(report: &serde_json::Value) -> u64 {
    report["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["bytes"].as_u64())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(report: Vec<u8>) -> serde_json::Value {
        serde_json::from_slice(&report).expect("a report is JSON")
    }

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
        let report = parsed(inventory(&directory(&root)).expect("inventory"));
        assert_eq!(
            report["entries"],
            serde_json::json!([]),
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
        let before = parsed(inventory(&directory(&root)).expect("inventory"));
        assert_eq!(
            before["entries"][0]["bytes"], 4096,
            "an inventory must size each compiled runtime"
        );
        let cleared = parsed(clear(&directory(&root)).expect("clear"));
        assert_eq!(
            (
                &cleared["removed_bytes"],
                &cleared["entries"],
                &cleared["failures"]
            ),
            (
                &serde_json::json!(4096),
                &serde_json::json!([]),
                &serde_json::json!([])
            ),
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
