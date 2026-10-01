mod common;

use isoplane_prepare::{BuildRequest, LocalPreparation, Preparation, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const CACHE_SUBDIRECTORY: &str = "cache";

fn setup() -> (tempfile::TempDir, LocalPreparation) {
    let directory = tempfile::tempdir().expect("owned compiler cache");
    // SAFETY: this unique empty directory is written only by the cache's atomic compiler path.
    let cache = unsafe {
        isoplane_engine::CompilationCache::from_trusted_directory(
            directory.path().join(CACHE_SUBDIRECTORY),
        )
    }
    .expect("owned cache");
    let provider = LocalPreparation::with_cache(Arc::new(cache)).expect("cached local provider");
    (directory, provider)
}
fn wasm(value: u32) -> Vec<u8> {
    common::guest(
        r#"(import "isoplane" "init_hash_seed" (func $seed (param i32 i32)))
        (import "isoplane" "init_config_get" (func $config (param i32 i32) (result i32)))"#,
        &format!("(call $seed (i32.const 64) (i32.const 16)) (drop (call $config (i32.const 128) (i32.const 32))) (global.set $state (i32.const {value}))"),
    )
}
fn request(wasm: &[u8]) -> BuildRequest {
    BuildRequest {
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        application_files: isoplane_engine::PlatformFiles::default(),
        engine: common::engine().configuration(),
        runtime: RuntimeProfile::raw_wasm(),
        limits: isoplane_abi::config::Limits::default(),
        initialization: common::request(),
        input_hash: *blake3::hash(wasm).as_bytes(),
        portable_template: isoplane_prepare::PortableTemplate::Returned,
    }
}
fn files(directory: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(directory.join(CACHE_SUBDIRECTORY))
        .expect("cache entries")
        .map(|entry| entry.expect("cache entry").path())
        .collect();
    paths.sort();
    paths
}

#[cfg(unix)]
#[tokio::test]
async fn compiled_input_is_reused_while_each_initialization_draws_a_fresh_seed_and_applies_configuration(
) {
    use std::os::unix::fs::MetadataExt;
    let (directory, provider) = setup();
    let wasm = wasm(42);
    let mut request = request(&wasm);
    request.initialization.config = b"first".to_vec();
    let first = provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("first preparation");
    let entries = files(directory.path());
    assert_eq!(
        entries.len(),
        2,
        "one instrumented input produces one metadata/native pair"
    );
    let inodes: Vec<_> = entries
        .iter()
        .map(|path| std::fs::metadata(path).expect("cache inode").ino())
        .collect();
    request.initialization.config = b"second".to_vec();
    let second = provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("new initialization context");
    for (path, inode) in entries.iter().zip(inodes) {
        assert_eq!(
            std::fs::metadata(path).expect("reused inode").ino(),
            inode,
            "a cache hit must not replace its compiled input with another compile"
        );
    }
    let mut seeds = Vec::new();
    for (product, config) in [(first, b"first".as_slice()), (second, b"second".as_slice())] {
        let engine = common::engine();
        let template = engine
            .load_template(
                product
                    .portable_template()
                    .expect("a requested portable template is returned"),
            )
            .expect("initialized output");
        let memory = template.memory_image_arc().to_dense();
        seeds.push(memory[64..80].to_vec());
        assert_eq!(
            &memory[128..128 + config.len()],
            config,
            "every initialization must apply its own configuration"
        );
    }
    assert_ne!(
        seeds[0], seeds[1],
        "compilation reuse must not reuse initialized random state"
    );
}

#[tokio::test]
async fn changed_transformed_input_uses_another_compilation_key() {
    let (directory, provider) = setup();
    for value in [41, 42] {
        let wasm = wasm(value);
        provider
            .prepare(&wasm, &request(&wasm), &CancellationToken::new())
            .await
            .expect("distinct source preparation");
    }
    assert_eq!(
        files(directory.path()).len(),
        4,
        "different transformed modules must retain separate native entries"
    );
}

#[tokio::test]
async fn changed_engine_configuration_uses_another_compilation_key() {
    let (directory, provider) = setup();
    let wasm = wasm(42);
    let mut request = request(&wasm);
    provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("initial engine");
    request.engine.guard_size_bytes *= 2;
    provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("another engine profile");
    assert_eq!(
        files(directory.path()).len(),
        4,
        "incompatible engines must never consume the same native entry"
    );
}

#[tokio::test]
async fn a_partial_cache_entry_is_recompiled_before_use() {
    let (directory, provider) = setup();
    let wasm = wasm(42);
    let request = request(&wasm);
    provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("initial cache fill");
    let metadata = files(directory.path())
        .into_iter()
        .find(|path| path.extension().is_some_and(|ext| ext == "meta"))
        .expect("metadata path");
    std::fs::remove_file(&metadata).expect("simulate incomplete publication");
    let product = provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("partial entry repair");
    assert_eq!(
        product.initialized.seed_reads, 1,
        "a repaired entry must initialize successfully"
    );
    assert!(
        metadata.is_file(),
        "repair must republish a complete metadata/native pair"
    );
}

#[tokio::test]
async fn malformed_cache_metadata_is_refused_without_recompilation() {
    let (directory, provider) = setup();
    let wasm = wasm(42);
    let request = request(&wasm);
    provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("initial cache fill");
    let metadata = files(directory.path())
        .into_iter()
        .find(|path| path.extension().is_some_and(|ext| ext == "meta"))
        .expect("metadata path");
    std::fs::write(&metadata, b"invalid")
        .expect("malformed test metadata, with no live mapped entry");
    let error = provider
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .err()
        .expect("corrupt metadata must fail closed");
    assert!(
        matches!(
            error,
            isoplane_prepare::PrepareError::Rejected(isoplane_prepare::WorkerFailure {
                stage: isoplane_prepare::BuildStage::Initialization,
                ..
            })
        ),
        "a malformed cache entry must refuse initialization"
    );
    assert_eq!(
        std::fs::read(&metadata).expect("refused entry"),
        b"invalid",
        "refusal must not silently replace a corrupted cache entry"
    );
}

#[test]
fn preparation_identity_binds_the_independent_memory_authorization() {
    let wasm = wasm(42);
    let mut input = request(&wasm);
    let before = input.identity().expect("original identity");
    input.guest_memory_bytes =
        Some(isoplane_abi::config::GuestMemoryBytes::new(512 << 20).expect("valid budget"));
    assert_ne!(
        before,
        input.identity().expect("new identity"),
        "different memory authorizations must not reuse the same prepared identity"
    );
    assert_eq!(
        input.limits,
        isoplane_abi::config::Limits::default(),
        "memory authorization must not change CPU or resource plans"
    );
}

#[test]
fn preparation_rejects_memory_authorization_above_worker_capacity() {
    let wasm = wasm(42);
    let mut input = request(&wasm);
    input.engine.max_memory_bytes = 128 << 20;
    input.guest_memory_bytes =
        Some(isoplane_abi::config::GuestMemoryBytes::new(512 << 20).expect("valid budget"));
    let result = isoplane_prepare::prepare_in_current_process(&wasm, &input);
    assert!(
        matches!(result, Err(ref failure) if failure.stage == isoplane_prepare::BuildStage::Input),
        "an incompatible worker must refuse preparation before executing the guest"
    );
}

#[test]
fn structural_admission_policy_binds_preparation_identity_and_is_enforced() {
    let wasm = wasm(42);
    let mut input = request(&wasm);
    let before = input.identity().expect("original identity");
    input.validation_limits.max_functions = 0;
    assert_ne!(
        before,
        input.identity().expect("restricted identity"),
        "changing structural admission policy must invalidate prepared cache identity"
    );
    let result = isoplane_prepare::prepare_in_current_process(&wasm, &input);
    assert!(matches!(result, Err(ref failure) if failure.stage == isoplane_prepare::BuildStage::Input),
        "the preparation worker must reject functions forbidden by the supplied policy before initialization");
}

#[test]
fn initialization_without_a_memory_maximum_obeys_the_application_budget() {
    let budget_bytes = isoplane_abi::config::Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_http_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .limits()
    .instance_memory_max;
    let budget_pages = budget_bytes / (64 << 10);
    let wasm = wat::parse_str(format!(
        r#"(module
        (memory (export "memory") 1)
        (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 0)
        (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_initialize") (result i64)
            (if (i32.ne (memory.grow (i32.const {budget_pages})) (i32.const -1)) (then unreachable)) i64.const 0))"#
    ))
    .expect("bounded initialization fixture assembles");
    let mut input = request(&wasm);
    input.engine.max_memory_bytes = 2 * budget_bytes;
    input.limits = isoplane_abi::config::Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_http_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    };
    assert!(
        isoplane_prepare::prepare_in_current_process(&wasm, &input).is_ok(),
        "initialization must refuse growth past the Free plan's {budget_bytes}-byte budget on an engine allowing twice that"
    );
}

#[test]
fn preparation_identity_binds_the_plan_memory_default() {
    let wasm = wasm(42);
    let mut input = request(&wasm);
    let standard = input.identity().expect("standard identity");
    input.limits = isoplane_abi::config::Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_http_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    };
    assert_ne!(
        standard,
        input.identity().expect("free identity"),
        "different implicit memory budgets must not share preparation identity"
    );
}

#[test]
fn preparation_identity_binds_the_guest_visible_engine_memory_ceiling() {
    let wasm = wasm(42);
    let mut input = request(&wasm);
    let before = input.identity().expect("original memory identity");
    input.engine.max_memory_bytes *= 2;
    assert_ne!(before, input.identity().expect("changed memory identity"),
        "a runtime initialized with another visible memory ceiling must not reuse captured GC state");
}
