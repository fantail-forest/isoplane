//! A runtime prepared once to the end of its own initializer, and
//! applications prepared as layers over it.
mod common;
#[path = "common/guest.rs"]
mod guest;

use isoplane_engine::{CompilationCache, Template};
use isoplane_harness::preparation::{load, staged_guest};
use isoplane_prepare::{
    BuildRequest, PrepareError, PreparedApplication, PreparedBaseline, RuntimeBaseline,
    RuntimeProfile,
};

const ENTROPY: &str = r#"(import "isoplane" "entropy_fill" (func $entropy (param i32 i32)))"#;

/// Writes the runtime's marker into the first page.
const RUNTIME: &str = "(i64.store (i32.const 100) (i64.const 0x656d69746e7572))";

/// Sets `$state` and writes the application's marker into the second page.
const APPLICATION: &str = "(global.set $state (i32.const 42))
    (i64.store (i32.const 70000) (i64.const 0x6170706c))";

fn request(wasm: &[u8]) -> BuildRequest {
    BuildRequest {
        engine: common::engine_config(),
        application_files: Default::default(),
        runtime: RuntimeProfile::RawWasm,
        memory_limit: isoplane_abi::config::GuestMemoryBytes::default(),
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        initialization: common::request(),
        module: isoplane_prepare::Module::new(wasm),
    }
}

/// A runtime baseline prepared through an empty compilation cache of its
/// own, with the directory holding that cache, which its template maps code
/// from and its applications keep captured state in.
struct Baseline {
    prepared: PreparedBaseline,
    directory: tempfile::TempDir,
}

impl Baseline {
    fn application(&self, request: &BuildRequest) -> Result<PreparedApplication, PrepareError> {
        self.prepared
            .baseline
            .prepare_application_in(request, self.directory.path())
    }
}

fn temporary_cache() -> (tempfile::TempDir, CompilationCache) {
    let directory = tempfile::tempdir().expect("an isolated compilation cache");
    // SAFETY: the cache creates this directory privately, empty, for this test,
    // and only CompilationCache's compiler path writes to it.
    let cache = unsafe { CompilationCache::from_trusted_directory(directory.path().join("cache")) }
        .expect("create the compilation cache");
    (directory, cache)
}

async fn baseline(request: &BuildRequest) -> Result<Baseline, PrepareError> {
    let (directory, cache) = temporary_cache();
    let prepared = RuntimeBaseline::prepare(&request.baseline(), &cache)?;
    Ok(Baseline {
        prepared,
        directory,
    })
}

fn refusal_of(result: Result<Baseline, PrepareError>) -> PrepareError {
    match result {
        Ok(_) => panic!("the runtime baseline must be refused"),
        Err(error) => error,
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_application_over_a_baseline_loads_to_the_memory_its_whole_initialization_leaves() {
    use isoplane_prepare::Preparation;
    let wasm = staged_guest(ENTROPY, RUNTIME, APPLICATION);
    let request = request(&wasm);
    let preparation = isoplane_harness::preparation::local_preparation();
    let whole = preparation
        .prepare(&request, &tokio_util::sync::CancellationToken::new())
        .await
        .expect("the whole initialization must prepare");
    let runtime = baseline(&request)
        .await
        .expect("the runtime stage must prepare as a baseline");
    let layer = runtime
        .application(&request)
        .expect("the application must prepare over its baseline")
        .product;
    let engine = common::engine();
    let identity = request.identity().expect("the application's identity");
    let whole = load(&engine, &whole, identity, None);
    let base = load(
        &engine,
        &runtime.prepared.product,
        request
            .baseline()
            .identity()
            .expect("the baseline's identity"),
        None,
    );
    let layer = load(&engine, &layer, identity, Some(&base));
    assert_eq!(
        layer.memory_image_arc().to_dense(),
        whole.memory_image_arc().to_dense(),
        "an application layer over its runtime baseline must hold the memory its whole initialization leaves"
    );
}

#[tokio::test]
async fn a_module_without_a_runtime_stage_cannot_produce_a_baseline() {
    let wasm = guest::guest(ENTROPY, "");
    let refusal = refusal_of(baseline(&request(&wasm)).await);
    assert!(
        matches!(refusal, PrepareError::InvalidRequest(_)),
        "an absent runtime stage must not produce an uninitialized baseline: {refusal}"
    );
}

#[tokio::test]
async fn a_module_without_an_application_initializer_cannot_produce_a_baseline() {
    let wasm = wat::parse_str(
        r#"(module
        (memory (export "memory") 1 1)
        (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 0)
        (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) i64.const 0)
        (func (export "isoplane_initialize_runtime") (result i64) i64.const 0))"#,
    )
    .expect("runtime-only fixture");
    let refusal = refusal_of(baseline(&request(&wasm)).await);
    assert!(
        matches!(refusal, PrepareError::InvalidRequest(_)),
        "a runtime without an application initializer must not produce a deployable baseline: {refusal}"
    );
}

#[tokio::test]
async fn a_runtime_stage_that_draws_entropy_is_refused_as_a_baseline() {
    let wasm = staged_guest(ENTROPY, "(call $entropy (i32.const 64) (i32.const 16))", "");
    let refusal = refusal_of(baseline(&request(&wasm)).await);
    assert!(
        matches!(refusal, PrepareError::Baseline(_)),
        "entropy drawn into a baseline would be shared by every application on it: {refusal}"
    );
}

#[test]
fn a_second_baseline_reuses_the_compilation_the_first_left_in_its_cache() {
    let wasm = staged_guest(ENTROPY, RUNTIME, APPLICATION);
    let request = request(&wasm).baseline();
    let (_directory, cache) = temporary_cache();
    let compiled = || {
        RuntimeBaseline::prepare(&request, &cache)
            .expect("the runtime stage must prepare as a baseline")
            .product
            .initialized
            .compiled
    };
    assert_eq!(
        (compiled(), compiled()),
        (true, false),
        "the first baseline must compile the runtime into the cache it is given and the second must reuse that"
    );
}

async fn layer_refusal(runtime: &Baseline, request: &BuildRequest) -> PrepareError {
    match runtime.application(request) {
        Ok(_) => panic!("the layer must be refused"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn an_application_is_refused_over_a_baseline_prepared_for_other_limits() {
    let wasm = staged_guest(ENTROPY, RUNTIME, APPLICATION);
    let request = request(&wasm);
    let runtime = baseline(&request).await.expect("the baseline");
    let other = BuildRequest {
        guest_memory_bytes: Some(
            isoplane_abi::config::GuestMemoryBytes::new(2 << 20).expect("a memory limit"),
        ),
        ..request
    };
    let refusal = layer_refusal(&runtime, &other).await;
    assert!(
        matches!(refusal, PrepareError::Input(_)),
        "a layer must be prepared over the baseline its own runtime, engine and limits derive: {refusal}"
    );
}

/// Local preparation over a published runtime baseline and its manifest receipt.
#[cfg(target_os = "linux")]
mod published {
    use isoplane_engine::IsoplaneEngine;
    use isoplane_harness::preparation::load_in;
    use isoplane_prepare::{Preparation, PublishedBaseline};
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// `request`'s runtime baseline, prepared through `preparation` and published
    /// in `directory` as a local baseline, with the digest of its published
    /// manifest.
    ///
    /// # Safety
    /// Nothing may change the files in `directory` while the returned
    /// baseline, the template or anything prepared or loaded over them exists.
    async unsafe fn published_baseline(
        preparation: &dyn Preparation,
        engine: &IsoplaneEngine,
        request: &BuildRequest,
        directory: &std::path::Path,
    ) -> (PublishedBaseline, Template) {
        let product = preparation
            .prepare_runtime_baseline(&request.baseline(), &CancellationToken::new())
            .await
            .expect("the runtime stage must prepare as a baseline");

        let identity = request
            .baseline()
            .identity()
            .expect("the baseline's identity");
        // SAFETY: this function's caller keeps the files in `directory` unchanged.
        let base = unsafe { load_in(engine, &product, identity, None, directory) };
        let digest = engine
            .prepared_manifest_sha256(directory)
            .expect("the published manifest digest");
        // SAFETY: this test's LocalPreparation produced the baseline, and this
        // function's caller keeps the files in `directory` unchanged.
        (
            unsafe { PublishedBaseline::new(directory.to_path_buf(), digest) },
            base,
        )
    }

    #[tokio::test]
    async fn an_application_prepared_over_a_published_baseline_loads_to_its_whole_initialization() {
        let wasm = staged_guest(ENTROPY, RUNTIME, APPLICATION);
        let request = request(&wasm);
        let preparation = isoplane_harness::preparation::local_preparation();
        let engine = common::engine();
        let directory = tempfile::tempdir().expect("publication");
        // SAFETY: `directory` is this test's own, and the test changes
        // nothing in it once the baseline is published there.
        let (published, base) = unsafe {
            published_baseline(
                &*preparation,
                &engine,
                &request,
                &directory.path().join("base"),
            )
        }
        .await;
        let layer = preparation
            .prepare_over(&published, &request, &CancellationToken::new())
            .await
            .expect("the application must prepare over the published baseline");
        let whole = preparation
            .prepare(&request, &CancellationToken::new())
            .await
            .expect("the whole initialization must prepare");
        let identity = request.identity().expect("the application's identity");
        let layer = load(&engine, &layer, identity, Some(&base));
        let whole = load(&engine, &whole, identity, None);
        assert_eq!(
            layer.memory_image_arc().to_dense(),
            whole.memory_image_arc().to_dense(),
            "a layer prepared over a published baseline must hold the memory its whole initialization leaves"
        );
    }

    #[tokio::test]
    async fn an_application_is_refused_when_the_supplied_baseline_digest_differs() {
        let wasm = staged_guest(ENTROPY, RUNTIME, APPLICATION);
        let request = request(&wasm);
        let preparation = isoplane_harness::preparation::local_preparation();
        let directory = tempfile::tempdir().expect("publication");
        // SAFETY: `directory` is this test's own, and the test changes
        // nothing in it once the baseline is published there.
        let (_published, _base) = unsafe {
            published_baseline(
                &*preparation,
                &common::engine(),
                &request,
                &directory.path().join("base"),
            )
        }
        .await;
        // SAFETY: this test's LocalPreparation produced the baseline, which
        // the test leaves unchanged in its own directory.
        let other = unsafe { PublishedBaseline::new(directory.path().join("base"), [0; 32]) };
        match preparation
            .prepare_over(&other, &request, &CancellationToken::new())
            .await
        {
            Ok(_) => panic!("a baseline without its recorded digest must be refused"),
            Err(error) => assert!(
                matches!(error, PrepareError::Input(_)),
                "a baseline with a different supplied manifest digest must be refused before its native code loads: {error}"
            ),
        }
    }

    #[tokio::test]
    async fn an_engine_that_cannot_be_constructed_is_reported_as_that_over_a_published_baseline() {
        let wasm = staged_guest(ENTROPY, RUNTIME, APPLICATION);
        let request = request(&wasm);
        let preparation = isoplane_harness::preparation::local_preparation();
        let directory = tempfile::tempdir().expect("publication");
        // SAFETY: `directory` is this test's own, and the test changes
        // nothing in it once the baseline is published there.
        let (published, _base) = unsafe {
            published_baseline(
                &*preparation,
                &common::engine(),
                &request,
                &directory.path().join("base"),
            )
        }
        .await;
        let mut unconstructible = request;
        unconstructible.engine.max_wasm_stack_bytes = 0;

        let Err(over) = preparation
            .prepare_over(&published, &unconstructible, &CancellationToken::new())
            .await
        else {
            panic!("an application must not prepare under an engine that cannot be constructed")
        };
        assert!(
            matches!(over, PrepareError::Engine(_)),
            "over a valid published baseline, an engine that cannot be constructed must not be reported as a bad baseline: {over:?}"
        );
        let Err(baseline) = preparation
            .prepare_runtime_baseline(&unconstructible.baseline(), &CancellationToken::new())
            .await
        else {
            panic!("a baseline must not prepare under an engine that cannot be constructed")
        };
        assert!(
            matches!(baseline, PrepareError::Engine(_)),
            "preparing the baseline must report the same cause: {baseline:?}"
        );
    }
}

/// A guest whose `isoplane_gc_hint` runs `hint`, and whose application
/// initializer records at 308 how many collections, counted at 300, ran
/// before it; with `runtime_stage`, it also has an empty runtime stage.
fn collecting_guest(runtime_stage: bool, hint: &str) -> Vec<u8> {
    let runtime = if runtime_stage {
        r#"(func (export "isoplane_initialize_runtime") (result i64) i64.const 0)"#
    } else {
        ""
    };
    wat::parse_str(format!(
        r#"(module
      (memory (export "memory") 2 64)
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_gc_hint") (param $pressure i32) {hint})
      {runtime}
      (func (export "isoplane_initialize") (result i64)
        (i32.store (i32.const 308) (i32.load (i32.const 300)))
        i64.const 0))"#
    ))
    .expect("the collecting test guest must assemble")
}

/// Counts a collection at 300 and records its pressure at 304.
const COUNT_COLLECTION: &str =
    "(i32.store (i32.const 300) (i32.add (i32.load (i32.const 300)) (i32.const 1)))
    (i32.store (i32.const 304) (local.get $pressure))";

/// The little-endian word at `offset` of `template`'s memory.
fn word(template: &Template, offset: usize) -> u32 {
    let memory = template.memory_image_arc().to_dense();
    u32::from_le_bytes(memory[offset..offset + 4].try_into().expect("four bytes"))
}

struct Layered {
    base: Template,
    layer: Template,
}

/// Prepares `wasm`'s baseline and its application layer over it, loaded.
async fn baseline_and_layer(wasm: &[u8]) -> Layered {
    let request = request(wasm);
    let runtime = baseline(&request)
        .await
        .expect("the runtime stage must prepare as a baseline");
    let layer = runtime
        .application(&request)
        .expect("the application must prepare over its baseline")
        .product;
    let engine = common::engine();
    let base = load(
        &engine,
        &runtime.prepared.product,
        request
            .baseline()
            .identity()
            .expect("the baseline's identity"),
        None,
    );
    let layer = load(
        &engine,
        &layer,
        request.identity().expect("the application's identity"),
        Some(&base),
    );
    Layered { base, layer }
}

#[tokio::test]
async fn each_initializer_stage_collects_the_guests_garbage_before_its_capture() {
    let wasm = collecting_guest(true, COUNT_COLLECTION);
    let Layered { base, layer } = baseline_and_layer(&wasm).await;
    assert_eq!(
        (word(&base, 300), word(&layer, 308), word(&layer, 300)),
        (1, 1, 2),
        "the baseline must be captured after the runtime stage's collection, and the layer after the application's"
    );
    assert_eq!(
        word(&layer, 304),
        u32::from(isoplane_host::hibernate_capture::GC_HINT_SNAPSHOT),
        "a template capture must ask for the collection a hibernation capture does"
    );
}

#[tokio::test]
async fn a_guest_that_traps_while_collecting_fails_its_preparation() {
    let wasm = collecting_guest(true, "unreachable");
    let failure = match baseline(&request(&wasm)).await {
        Ok(_) => panic!("a guest that traps while collecting must not be captured"),
        Err(failure) => failure,
    };
    assert!(
        matches!(
            failure,
            PrepareError::Initialization {
                completion: isoplane_engine::Completion::Trapped,
                ..
            }
        ),
        "a trap in the collection before capture must fail initialization with the guest's trap as its outcome: {failure}"
    );
}
