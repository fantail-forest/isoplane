//! The engine configuration tests load the shared runtime under, and that
//! runtime as `task fixtures:compiled-runtime` compiled it for them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use isoplane_engine::{CompilationCache, EngineConfig, IsoplaneEngine, Template};

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
    let path = std::env::var_os("ISOPLANE_TEST_COMPILED_RUNTIME")
        .map(PathBuf::from)
        .expect("run task fixtures:compiled-runtime to select the compiled shared runtime");
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

/// The compiled artifacts of [`instrumented_runtime`] for [`engine_config`],
/// in the layout a node's artifact store and a [`CompilationCache`] share.
pub fn compiled_runtime_artifacts() -> PathBuf {
    directory()
}

/// A cache holding [`compiled_runtime_artifacts`] for preparation, which also
/// keeps working files beside the artifacts it loads. It is a writable
/// directory in the target directory whose entries link to Task's.
pub fn compiled_runtime_cache() -> Arc<CompilationCache> {
    static CACHE: OnceLock<Arc<CompilationCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let cache = crate::target_directory().join("test-preparation-cache");
            CompilationCache::create_directory(&cache).expect("create the test preparation cache");
            link_artifacts(&directory(), &cache);
            // SAFETY: its entries are links to what `task fixtures:compiled-runtime` wrote
            // through `IsoplaneEngine::precompile_to_files`, and preparation in tests writes
            // here only through the same compiler path.
            let cache = unsafe { CompilationCache::from_trusted_directory(cache) }
                .expect("open the test preparation cache");
            Arc::new(cache)
        })
        .clone()
}

/// Makes `cache`'s compiled artifacts those in `artifacts`, so links to a
/// runtime Task has since replaced do not keep its files alive.
fn link_artifacts(artifacts: &Path, cache: &Path) {
    let compiled = |path: &Path| {
        path.extension()
            .is_some_and(|extension| extension == "cwasm" || extension == "meta")
    };
    for entry in std::fs::read_dir(cache).expect("list the test preparation cache") {
        let path = entry.expect("read the test preparation cache").path();
        if compiled(&path)
            && !artifacts
                .join(path.file_name().expect("an entry has a name"))
                .exists()
        {
            std::fs::remove_file(&path)
                .unwrap_or_else(|e| panic!("remove the outdated {}: {e}", path.display()));
        }
    }
    for entry in std::fs::read_dir(artifacts).expect("list the compiled shared runtime") {
        let source = entry.expect("read the compiled shared runtime").path();
        let target = cache.join(source.file_name().expect("an entry has a name"));
        if !compiled(&source) || target.exists() {
            continue;
        }
        std::fs::hard_link(&source, &target)
            .or_else(|_| std::fs::copy(&source, &target).map(drop))
            .unwrap_or_else(|e| panic!("link {}: {e}", target.display()));
    }
}

/// [`instrumented_runtime`] loaded on `engine` from the artifacts Task
/// compiled. Fails when `engine` was not configured by [`engine_config`].
pub fn load_compiled_runtime(engine: &IsoplaneEngine) -> Template {
    // SAFETY: only `task fixtures:compiled-runtime` writes this directory, through
    // `IsoplaneEngine::precompile_to_files`, and then makes it read-only, so an engine
    // configured otherwise fails to compile into it instead of quietly compiling for itself.
    let artifacts = unsafe { CompilationCache::from_trusted_directory(directory()) }
        .expect("open the compiled shared runtime");
    let (template, compiled) = artifacts
        .load(engine, &instrumented_runtime())
        .unwrap_or_else(|error| {
            panic!(
                "the shared runtime was not compiled for this engine; configure it with \
                 isoplane_fixtures::engine_config, or rerun task fixtures:compiled-runtime: {error:#}"
            )
        });
    assert!(
        !compiled,
        "the shared runtime compiled at test time; rerun task fixtures:compiled-runtime"
    );
    template
}
