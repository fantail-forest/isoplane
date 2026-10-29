//! The engine configuration tests load the shared runtime under, and that
//! runtime as `task fixtures:compiled-runtime` compiled it for them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use isoplane_engine::{ArtifactPaths, CompilationCache, EngineConfig, IsoplaneEngine, Template};

/// Pool tables for every test engine. Fixing them lets each test choose its
/// pool size without changing the code Task compiled.
const TOTAL_TABLES: u32 = 64;

/// The engine every test that loads the shared runtime uses, with room for
/// `max_instances` instances: a node's defaults, compiling for this host's
/// processor features. Fields the compiled code does not depend
/// on, such as platform files, may be changed.
pub fn engine_config(max_instances: u32) -> EngineConfig {
    assert!(
        max_instances <= TOTAL_TABLES,
        "a test engine holds at most {TOTAL_TABLES} instances"
    );
    EngineConfig {
        max_instances,
        total_tables: Some(TOTAL_TABLES),
        ..EngineConfig::default()
    }
}

fn directory() -> PathBuf {
    let path = isoplane_test_support::prepared_path(
        "ISOPLANE_TEST_COMPILED_RUNTIME",
        "task fixtures:compiled-runtime",
    );
    assert!(
        path.join("runtime.wasm").is_file(),
        "the compiled shared runtime is missing at {}; run task fixtures:compiled-runtime",
        path.display()
    );
    path
}

/// The shared runtime as preparation instruments it, which is the module a
/// deploy compiles and serves.
pub fn instrumented_runtime() -> Vec<u8> {
    crate::read(&directory().join("runtime.wasm"))
}

/// A cache holding the compiled shared runtime for preparation, which also
/// keeps working files beside the artifacts it loads. It is a writable
/// directory in the target directory whose entries link to Task's.
pub fn compiled_runtime_cache() -> Arc<CompilationCache> {
    static CACHE: OnceLock<Arc<CompilationCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let cache = crate::target_directory().join("test-preparation-cache");
            CompilationCache::create_directory(&cache).expect("create the test preparation cache");
            unlink_replaced_artifacts(&directory(), &cache);
            link_compiled_runtime(&cache);
            // SAFETY: its entries are links to what `task fixtures:compiled-runtime` wrote
            // through `IsoplaneEngine::precompile_to_files`, and preparation in tests writes
            // here only through the same compiler path.
            let cache = unsafe { CompilationCache::from_trusted_directory(cache) }
                .expect("open the test preparation cache");
            Arc::new(cache)
        })
        .clone()
}

/// Gives the artifact store at `store` the compiled artifacts of
/// [`instrumented_runtime`] for [`engine_config`], in the layout a node's
/// artifact store and a [`CompilationCache`] share, so that a deploy there
/// serves the shared runtime without compiling it again. Entries `store`
/// already holds stay.
pub fn link_compiled_runtime(store: &Path) {
    isoplane_test_support::link_files(&directory(), store, is_compiled_artifact);
}

fn is_compiled_artifact(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "cwasm" || extension == "meta")
}

/// Removes from `cache` the compiled artifacts `artifacts` no longer holds,
/// so links to a runtime Task has since replaced do not keep its files alive.
fn unlink_replaced_artifacts(artifacts: &Path, cache: &Path) {
    for entry in std::fs::read_dir(cache).expect("list the test preparation cache") {
        let path = entry.expect("read the test preparation cache").path();
        if is_compiled_artifact(&path)
            && !artifacts
                .join(path.file_name().expect("an entry has a name"))
                .exists()
        {
            std::fs::remove_file(&path)
                .unwrap_or_else(|e| panic!("remove the outdated {}: {e}", path.display()));
        }
    }
}

/// [`instrumented_runtime`] loaded on `engine` from the artifacts Task
/// compiled. Fails, before compiling anything into Task's directory, when
/// `engine` was not configured by [`engine_config`].
pub fn load_compiled_runtime(engine: &IsoplaneEngine) -> Template {
    let wasm = instrumented_runtime();
    let directory = directory();
    let paths = ArtifactPaths::new(&directory, &engine.artifact_key(&wasm));
    assert!(
        paths.meta.is_file() && paths.cwasm.is_file(),
        "the shared runtime was not compiled for this engine; configure it with \
         isoplane_fixtures::engine_config, or rerun task fixtures:compiled-runtime"
    );
    // SAFETY: only `task fixtures:compiled-runtime` writes this directory, through
    // `IsoplaneEngine::precompile_to_files`, and the entry for this engine is present,
    // so loading maps it and compiles nothing.
    let artifacts = unsafe { CompilationCache::from_trusted_directory(directory) }
        .expect("open the compiled shared runtime");
    let (template, compiled) = artifacts
        .load(engine, &wasm)
        .expect("load the shared runtime Task compiled");
    assert!(
        !compiled,
        "the shared runtime compiled at test time; rerun task fixtures:compiled-runtime"
    );
    template
}
