mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use wisp_prepare::{BuildRequest, LocalPreparation, Preparation, RuntimeProfile};

const CACHE_SUBDIRECTORY: &str = "cache";

fn setup() -> (tempfile::TempDir, LocalPreparation) {
    let directory = tempfile::tempdir().expect("owned compiler cache");
    // SAFETY: this unique empty directory is written only by the cache's atomic compiler path.
    let cache = unsafe {
        wisp_engine::CompilationCache::from_trusted_directory(
            directory.path().join(CACHE_SUBDIRECTORY),
        )
    }
    .expect("owned cache");
    let provider = LocalPreparation::with_cache(Arc::new(cache)).expect("cached local provider");
    (directory, provider)
}
fn wasm(value: u32) -> Vec<u8> {
    common::guest(
        r#"(import "wisp" "init_hash_seed" (func $seed (param i32 i32)))
        (import "wisp" "init_config_get" (func $config (param i32 i32) (result i32)))"#,
        &format!("(call $seed (i32.const 64) (i32.const 16)) (drop (call $config (i32.const 128) (i32.const 32))) (global.set $state (i32.const {value}))"),
    )
}
fn request(wasm: &[u8]) -> BuildRequest {
    BuildRequest {
        guest_memory_bytes: None,
        application_files: wisp_engine::PlatformFiles::default(),
        engine: common::engine().configuration(),
        runtime: RuntimeProfile::raw_wasm(),
        limits: wisp_abi::config::Limits::default(),
        initialization: common::request(),
        input_hash: *blake3::hash(wasm).as_bytes(),
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
            .load_template(&product.initialized.wasm)
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
            wisp_prepare::PrepareError::Rejected(wisp_prepare::WorkerFailure {
                stage: wisp_prepare::BuildStage::Initialization,
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
        Some(wisp_abi::config::GuestMemoryBytes::new(512 << 20).expect("valid budget"));
    assert_ne!(
        before,
        input.identity().expect("new identity"),
        "different memory authorizations must not reuse the same prepared identity"
    );
    assert_eq!(
        input.limits,
        wisp_abi::config::Limits::default(),
        "memory authorization must not change CPU or resource plans"
    );
}

#[test]
fn preparation_rejects_memory_authorization_above_worker_capacity() {
    let wasm = wasm(42);
    let mut input = request(&wasm);
    input.engine.max_memory_bytes = 128 << 20;
    input.guest_memory_bytes =
        Some(wisp_abi::config::GuestMemoryBytes::new(512 << 20).expect("valid budget"));
    let result = wisp_prepare::prepare_in_current_process(&wasm, &input);
    assert!(
        matches!(result, Err(ref failure) if failure.stage == wisp_prepare::BuildStage::Input),
        "an incompatible worker must refuse preparation before executing the guest"
    );
}
