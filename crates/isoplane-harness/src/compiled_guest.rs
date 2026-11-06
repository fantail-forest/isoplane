//! Share disk-backed compilation while keeping engines and guest state isolated.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

use isoplane_cache::{Cache, CacheKey, PROJECT_CACHE_BYTES};
use isoplane_engine::{
    CompilationCache, EngineConfig, IsoplaneEngine, ModuleHash, PlatformFiles, Template,
};

struct CompiledGuests(Cache);

type CompilationLocks = Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>;

fn local_compilation_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<CompilationLocks> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Mutex::default).lock();
    locks.retain(|_, lock| lock.strong_count() != 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.into(), Arc::downgrade(&lock));
    lock
}

impl CompiledGuests {
    fn new(directory: PathBuf) -> Self {
        Self(
            Cache::new(directory, PROJECT_CACHE_BYTES)
                .expect("open bounded compiled-fixture storage"),
        )
    }

    fn load(&self, engine: &IsoplaneEngine, module: &[u8]) -> (Template, bool) {
        let lease = Arc::new(
            self.0
                .lease(CacheKey(ModuleHash::of(module).0))
                .expect("lease compiled guest storage"),
        );
        let path = lease.directory();
        // NFS file locks can be process-owned, so local writers also need a mutex.
        let local = local_compilation_lock(path);
        let _local = local.lock();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.join("build.lock"))
            .expect("open guest compilation lock");
        fs4::FileExt::lock(&lock).expect("serialize guest compilation across binaries");
        // SAFETY: only the compiler writes into this private, leased fixture directory.
        let cache = unsafe { CompilationCache::from_trusted_directory(path.into()) }
            .expect("open compiled guest cache");
        let (mut template, compiled) = cache
            .load(engine, module)
            .expect("compile the guest fixture");
        template.retain_storage(lease);
        (template, compiled)
    }
}

/// The engine every returned guest runs on, serving `files`. Preparation
/// output a returned guest loads must come from an engine configured alike.
pub fn engine_configuration(files: PlatformFiles) -> EngineConfig {
    // This harness holds at most two instances; node-sized pools only reserve unused slots.
    let config = EngineConfig {
        platform_files: files,
        ..isoplane_fixtures::engine_config(2)
    };
    assert_eq!(
        config.max_memory_bytes,
        isoplane_abi::config::memory_ceiling_bytes(
            isoplane_abi::config::DEFAULT_MEMORY_LIMIT_BYTES
        ),
        "returned guests' engine holds their application's memory ceiling"
    );
    config
}

pub(super) fn engine_serving(module: &[u8], files: PlatformFiles) -> (IsoplaneEngine, Template) {
    let engine = IsoplaneEngine::new(engine_configuration(files))
        .expect("the returned-guest engine must initialize");
    static COMPILED: OnceLock<CompiledGuests> = OnceLock::new();
    let cache = COMPILED.get_or_init(|| {
        CompiledGuests::new(isoplane_fixtures::cache_directory().join("compiled-tests"))
    });
    let template = cache.load(&engine, module).0;
    (engine, template)
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn fixture_tenancy_and_prepared_engine_share_the_memory_budget() {
        let reactor = crate::Reactor::new(isoplane_host::InstanceId(1));
        let engine = super::engine_configuration(Default::default());
        assert_eq!(
            isoplane_abi::config::memory_ceiling_bytes(reactor.context.limits.instance_memory_max),
            engine.max_memory_bytes,
            "fixture tenancy must fit the engine profile Task compiled"
        );
    }

    use super::*;
    use isoplane_engine::{HostBindings, ImportCall, InstanceOps};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn concurrent_cache_owners_compile_a_module_once() {
        let directory = tempfile::tempdir().expect("isolate the compiled-guest cache");
        let module = wat::parse_str("(module (memory (export \"memory\") 1))")
            .expect("the compilation fixture must parse");
        let compiled = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let cache = CompiledGuests::new(directory.path().join("cache"));
                    let engine =
                        IsoplaneEngine::new(EngineConfig::default()).expect("fixture engine");
                    let (_, did_compile) = cache.load(&engine, &module);
                    compiled.fetch_add(usize::from(did_compile), Ordering::Relaxed);
                });
            }
        });
        assert_eq!(
            compiled.load(Ordering::Relaxed),
            1,
            "independent cache owners must reuse the first completed compilation"
        );
    }

    struct NoImports;

    impl HostBindings for NoImports {
        fn begin_slice(&mut self) {}
        fn end_slice(&mut self) {}
        fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
            panic!("the memory fixture has no host imports")
        }

        fn call_sync(&mut self, _: ImportCall<'_>) -> i64 {
            panic!("the memory fixture has no host imports")
        }

        fn slice_clock_ms(&self) -> u64 {
            0
        }
    }

    #[test]
    fn reusing_compiled_code_keeps_guest_memory_independent() {
        let module = wat::parse_str(
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 0) "original")
                (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
                (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 0)
                (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0))"#,
        )
        .expect("the isolated-memory fixture must parse");
        let (first_engine, first) = engine_serving(&module, PlatformFiles::default());
        let mut first_instance = first_engine
            .activate(&first, Box::new(NoImports))
            .expect("the first compiled guest must activate");
        first_instance.memory()[..8].copy_from_slice(b"modified");
        let (second_engine, second) = engine_serving(&module, PlatformFiles::default());
        let mut second_instance = second_engine
            .activate(&second, Box::new(NoImports))
            .expect("a cached guest must activate in another engine");
        assert_eq!(
            &second_instance.memory()[..8],
            b"original",
            "sharing compiled code must not share a previous guest's mutable memory"
        );
        assert_eq!(
            &first_instance.memory()[..8],
            b"modified",
            "another activation must not reset a live guest's memory"
        );
    }
}
