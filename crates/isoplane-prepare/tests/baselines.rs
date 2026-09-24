//! A runtime prepared once to the end of its own initializer, and
//! applications prepared as layers over it.
#![cfg(target_os = "linux")]
mod common;
#[path = "common/staged.rs"]
mod staged;

use isoplane_abi::config::InitSandbox;
use isoplane_engine::prepared::PreparedIdentity;
use isoplane_engine::{IsoplaneEngine, Template};
use isoplane_prepare::{
    BuildProduct, BuildRequest, BuildStage, LocalPreparation, PortableTemplate, Preparation,
    PrepareError, PreparedBaseline, PublishedBaseline, RuntimeBaseline, RuntimeProfile,
    WorkerFailure,
};
use tokio_util::sync::CancellationToken;

const SEED: &str = r#"(import "isoplane" "init_hash_seed" (func $seed (param i32 i32)))"#;
const ENTROPY: &str = r#"(import "isoplane" "entropy_fill" (func $entropy (param i32 i32)))"#;

/// Writes the runtime's marker into the first page.
const RUNTIME: &str = "(i64.store (i32.const 100) (i64.const 0x656d69746e7572))";

/// Reads the application's seed into the first page, sets `$state` and
/// writes the application's marker into the second page.
const APPLICATION: &str = "(call $seed (i32.const 64) (i32.const 16))
    (global.set $state (i32.const 42))
    (i64.store (i32.const 70000) (i64.const 0x6170706c))";

fn request(wasm: &[u8], sandbox: InitSandbox) -> BuildRequest {
    BuildRequest {
        engine: common::engine().configuration(),
        application_files: Default::default(),
        runtime: RuntimeProfile::raw_wasm(),
        limits: isoplane_abi::config::Limits::default(),
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        initialization: isoplane_prepare::Initialization {
            sandbox,
            ..common::request()
        },
        input_hash: *blake3::hash(wasm).as_bytes(),
        portable_template: PortableTemplate::Omitted,
    }
}

async fn baseline(wasm: &[u8], request: &BuildRequest) -> Result<PreparedBaseline, WorkerFailure> {
    RuntimeBaseline::prepare(wasm, &request.baseline(), None)
}

fn stage_of(result: Result<PreparedBaseline, WorkerFailure>) -> BuildStage {
    match result {
        Ok(_) => panic!("the runtime baseline must be refused"),
        Err(failure) => failure.stage,
    }
}

/// Loads `product`, prepared as `identity`, into `engine`, over `base` when
/// it is a layer.
fn load(
    engine: &IsoplaneEngine,
    product: &BuildProduct,
    identity: PreparedIdentity,
    base: Option<&Template>,
    directory: &std::path::Path,
) -> Template {
    let bytes = product.prepared_bytes();
    // SAFETY: LocalPreparation in this process produced these bytes.
    let prepared = unsafe {
        match base {
            Some(base) => engine.decode_builder_output_over(base, bytes, identity),
            None => engine.decode_builder_output(bytes, identity),
        }
    }
    .expect("the prepared output must decode");
    prepared.publish(directory).expect("publish");
    // SAFETY: LocalPreparation in this process produced the decoded native code.
    // The caller holds this private publication directory and leaves its files
    // unchanged for every returned template and instance.
    unsafe {
        match base {
            Some(base) => engine.load_prepared_over(base, directory, identity, &[]),
            None => engine.load_prepared(directory, identity, &[]),
        }
    }
    .expect("the prepared output must load")
}

#[tokio::test]
async fn an_application_layer_preserves_initialized_state_outside_its_fresh_seed() {
    let wasm = staged::staged_guest(SEED, RUNTIME, APPLICATION);
    let request = request(&wasm, InitSandbox::Sealed);
    let preparation = LocalPreparation::new().expect("local preparation");
    let whole = preparation
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("the whole initialization must prepare");
    let runtime = baseline(&wasm, &request)
        .await
        .expect("the runtime stage must prepare as a baseline");
    let layer = runtime
        .baseline
        .prepare_application(&request)
        .expect("the application must prepare over its baseline")
        .product;
    let engine = common::engine();
    let directory = tempfile::tempdir().expect("publication");
    let identity = request.identity().expect("the application's identity");
    let whole = load(
        &engine,
        &whole,
        identity,
        None,
        &directory.path().join("whole"),
    );
    let base = load(
        &engine,
        &runtime.product,
        request
            .baseline()
            .identity()
            .expect("the baseline's identity"),
        None,
        &directory.path().join("base"),
    );
    let layer = load(
        &engine,
        &layer,
        identity,
        Some(&base),
        &directory.path().join("layer"),
    );
    // APPLICATION writes its independently drawn 16-byte seed only at 64..80.
    let layer_memory = layer.memory_image_arc().to_dense();
    let whole_memory = whole.memory_image_arc().to_dense();
    assert_eq!(
        (&layer_memory[..64], &layer_memory[80..]),
        (&whole_memory[..64], &whole_memory[80..]),
        "layered and whole initialization must preserve the same state outside the fresh application seed"
    );
}

#[tokio::test]
async fn a_module_without_a_runtime_stage_is_prepared_over_its_uninitialized_baseline() {
    let wasm = common::guest(SEED, "(i64.store (i32.const 200) (i64.const 0x6170706c))");
    let request = request(&wasm, InitSandbox::Sealed);
    let runtime = baseline(&wasm, &request)
        .await
        .expect("a module without a runtime stage must still prepare a baseline");
    let layer = runtime
        .baseline
        .prepare_application(&request)
        .expect("the application must prepare over its baseline")
        .product;
    let engine = common::engine();
    let directory = tempfile::tempdir().expect("publication");
    let base = load(
        &engine,
        &runtime.product,
        request
            .baseline()
            .identity()
            .expect("the baseline's identity"),
        None,
        &directory.path().join("base"),
    );
    let layer = load(
        &engine,
        &layer,
        request.identity().expect("the application's identity"),
        Some(&base),
        &directory.path().join("layer"),
    );
    assert_eq!(
        layer.memory_image_arc().to_dense()[200..204],
        *b"lppa",
        "the whole initialization must run over a baseline with no runtime stage"
    );
}

#[tokio::test]
async fn a_runtime_stage_that_reads_the_seed_is_refused_as_a_baseline() {
    let wasm = staged::staged_guest(SEED, "(call $seed (i32.const 64) (i32.const 16))", "");
    assert_eq!(
        stage_of(baseline(&wasm, &request(&wasm, InitSandbox::Sealed)).await),
        BuildStage::Baseline,
        "a seed read into a baseline would give every application on it the same entropy"
    );
}

#[tokio::test]
async fn a_runtime_stage_that_draws_entropy_is_refused_as_a_baseline() {
    let wasm = staged::staged_guest(ENTROPY, "(call $entropy (i32.const 64) (i32.const 16))", "");
    assert_eq!(
        stage_of(baseline(&wasm, &request(&wasm, InitSandbox::RuntimeBringUp)).await),
        BuildStage::Baseline,
        "entropy drawn into a baseline would be shared by every application on it"
    );
}

#[tokio::test]
async fn a_runtime_stage_reaching_a_refused_capability_is_refused_as_a_baseline() {
    let wasm = staged::staged_guest(ENTROPY, "(call $entropy (i32.const 64) (i32.const 16))", "");
    assert_eq!(
        stage_of(baseline(&wasm, &request(&wasm, InitSandbox::Sealed)).await),
        BuildStage::Baseline,
        "a runtime stage cannot defer work into every application's initialization"
    );
}

async fn layer_refusal(runtime: &RuntimeBaseline, request: &BuildRequest) -> BuildStage {
    match runtime.prepare_application(request) {
        Ok(_) => panic!("the layer must be refused"),
        Err(failure) => failure.stage,
    }
}

#[tokio::test]
async fn an_application_is_refused_over_a_baseline_prepared_for_other_limits() {
    let wasm = staged::staged_guest(SEED, RUNTIME, APPLICATION);
    let request = request(&wasm, InitSandbox::Sealed);
    let runtime = baseline(&wasm, &request).await.expect("the baseline");
    let other = BuildRequest {
        guest_memory_bytes: Some(
            isoplane_abi::config::GuestMemoryBytes::new(2 << 20).expect("a memory limit"),
        ),
        ..request
    };
    assert_eq!(
        layer_refusal(&runtime.baseline, &other).await,
        BuildStage::Input,
        "a layer must be prepared over the baseline its own runtime, engine and limits derive"
    );
}

#[tokio::test]
async fn an_application_over_a_baseline_cannot_return_a_portable_module() {
    let wasm = staged::staged_guest(SEED, RUNTIME, APPLICATION);
    let request = request(&wasm, InitSandbox::Sealed);
    let runtime = baseline(&wasm, &request).await.expect("the baseline");
    let portable = BuildRequest {
        portable_template: PortableTemplate::Returned,
        ..request
    };
    assert_eq!(
        layer_refusal(&runtime.baseline, &portable).await,
        BuildStage::Input,
        "a layer has no portable module to return"
    );
}

/// `wasm`'s runtime baseline, prepared through `preparation` and published as
/// a local baseline, with the digest of its published manifest.
async fn published_baseline(
    preparation: &dyn Preparation,
    engine: &IsoplaneEngine,
    wasm: &[u8],
    request: &BuildRequest,
    directory: &std::path::Path,
) -> (PublishedBaseline, Template) {
    let product = preparation
        .prepare_runtime_baseline(wasm, &request.baseline(), &CancellationToken::new())
        .await
        .expect("the runtime stage must prepare as a baseline");
    let identity = request
        .baseline()
        .identity()
        .expect("the baseline's identity");
    let base = load(engine, &product, identity, None, directory);
    let digest = engine
        .prepared_manifest_sha256(directory)
        .expect("the published manifest digest");
    // SAFETY: this test's LocalPreparation produced the private baseline;
    // its native code, state and mapped bytes remain unchanged while in use.
    (
        unsafe { PublishedBaseline::new(directory.to_path_buf(), digest) },
        base,
    )
}

#[tokio::test]
async fn a_published_baseline_layer_preserves_initialized_state_outside_its_fresh_seed() {
    let wasm = staged::staged_guest(SEED, RUNTIME, APPLICATION);
    let request = request(&wasm, InitSandbox::Sealed);
    let preparation = LocalPreparation::new().expect("local preparation");
    let engine = common::engine();
    let directory = tempfile::tempdir().expect("publication");
    let (published, base) = published_baseline(
        &preparation,
        &engine,
        &wasm,
        &request,
        &directory.path().join("base"),
    )
    .await;
    let layer = preparation
        .prepare_over(&published, &request, &CancellationToken::new())
        .await
        .expect("the application must prepare over the published baseline");
    let whole = preparation
        .prepare(&wasm, &request, &CancellationToken::new())
        .await
        .expect("the whole initialization must prepare");
    let identity = request.identity().expect("the application's identity");
    let layer = load(
        &engine,
        &layer,
        identity,
        Some(&base),
        &directory.path().join("layer"),
    );
    let whole = load(
        &engine,
        &whole,
        identity,
        None,
        &directory.path().join("whole"),
    );
    // APPLICATION writes its independently drawn 16-byte seed only at 64..80.
    let layer_memory = layer.memory_image_arc().to_dense();
    let whole_memory = whole.memory_image_arc().to_dense();
    assert_eq!(
        (&layer_memory[..64], &layer_memory[80..]),
        (&whole_memory[..64], &whole_memory[80..]),
        "published-baseline and whole initialization must preserve the same state outside the fresh application seed"
    );
}

#[tokio::test]
async fn an_application_is_refused_when_the_supplied_baseline_digest_differs() {
    let wasm = staged::staged_guest(SEED, RUNTIME, APPLICATION);
    let request = request(&wasm, InitSandbox::Sealed);
    let preparation = LocalPreparation::new().expect("local preparation");
    let directory = tempfile::tempdir().expect("publication");
    let (published, _base) = published_baseline(
        &preparation,
        &common::engine(),
        &wasm,
        &request,
        &directory.path().join("base"),
    )
    .await;
    // SAFETY: this test's LocalPreparation produced the private baseline;
    // its native code, state and mapped bytes remain unchanged while in use.
    let other = unsafe { PublishedBaseline::new(published.directory().to_path_buf(), [0; 32]) };
    match preparation
        .prepare_over(&other, &request, &CancellationToken::new())
        .await
    {
        Ok(_) => panic!("a baseline without its recorded digest must be refused"),
        Err(PrepareError::Rejected(failure)) => assert_eq!(
            failure.stage,
            BuildStage::Input,
            "a baseline with a different supplied manifest digest must be refused before its native code loads"
        ),
        Err(error) => panic!("the refusal must be the preparation's structured one: {error}"),
    }
}
