//! One Mono runtime module, linked and compiled with no application, runs
//! application packages it was never built with. Each package is served as
//! application files, loaded by the runtime's fixed loader during real
//! preparation, and executed through the reactor across a park and a
//! Store restoration.

#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/no_egress.rs"]
mod no_egress;

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use isoplane_abi::config::{InitSandbox, Limits};
use isoplane_abi::filesystem::APPLICATION_PACKAGE_ROOT;
use isoplane_engine::{CompilationCache, Completion, IsoplaneEngine, PlatformFile, PlatformFiles};
use isoplane_harness::EmittedGuestLogs as _;
use isoplane_harness::{engine_configuration, ReturnedGuest};
use isoplane_prepare::{
    BuildProduct, BuildRequest, BuildStage, Initialization, LocalPreparation, Preparation,
    PrepareError, RuntimeProfile,
};
use tokio_util::sync::CancellationToken;

/// A package directory as the host serves it: the manifest and assemblies
/// beneath the package root, any assets at the application's content root.
fn served(package: &Path) -> Vec<PlatformFile> {
    let mut files = Vec::new();
    let mut pending = vec![package.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("the package directory is readable") {
            let path = entry.expect("a package entry is readable").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(package)
                .expect("a package file lies beneath the package")
                .to_str()
                .expect("package paths are UTF-8")
                .to_owned();
            let served = match relative.strip_prefix("assets/") {
                Some(asset) => format!("/{asset}"),
                None => format!("{APPLICATION_PACKAGE_ROOT}/{relative}"),
            };
            files.push(PlatformFile {
                path: served,
                bytes: std::fs::read(&path)
                    .expect("a package file is readable")
                    .into(),
                modified_ns: 0,
            });
        }
    }
    files
}

fn package(name: &str) -> Vec<PlatformFile> {
    served(&isoplane_fixtures::mono_package(name))
}

/// `files` with the package file at `relative` replaced or added.
fn with_package_file(
    mut files: Vec<PlatformFile>,
    relative: &str,
    bytes: &[u8],
) -> Vec<PlatformFile> {
    let path = format!("{APPLICATION_PACKAGE_ROOT}/{relative}");
    files.retain(|file| file.path != path);
    files.push(PlatformFile {
        path,
        bytes: bytes.into(),
        modified_ns: 0,
    });
    files
}

fn package_file(files: &[PlatformFile], relative: &str) -> Vec<u8> {
    let path = format!("{APPLICATION_PACKAGE_ROOT}/{relative}");
    files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("the package serves {path}"))
        .bytes
        .to_vec()
}

fn request(runtime: &[u8], files: Vec<PlatformFile>) -> BuildRequest {
    BuildRequest {
        engine: engine_configuration(PlatformFiles::default()),
        application_files: PlatformFiles::new(files)
            .expect("package paths are valid application files"),
        runtime: RuntimeProfile::raw_wasm(),
        limits: Limits::default(),
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        initialization: Initialization {
            network: None,
            runtime_config: b"{}".to_vec(),
            hash_seed: [7; 16],
            config: b"{}".to_vec(),
            sandbox: InitSandbox::RuntimeBringUp,
            cpu_ms: 120_000,
            wall_ms: 300_000,
            max_passes: 4,
            log_bytes: 16 * 1024,
        },
        input_hash: *blake3::hash(runtime).as_bytes(),
    }
}

/// Compilations of the runtime shared by every preparation in this binary
/// that is not itself about compilation reuse.
fn shared_cache() -> Arc<CompilationCache> {
    static CACHE: OnceLock<Arc<CompilationCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let directory =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/compiled-test-packages");
            // SAFETY: this private test-output directory is written only through
            // CompilationCache's atomic compiler path, keyed by the portable module
            // and engine compatibility; nothing outside this binary supplies it.
            let cache = unsafe { CompilationCache::from_trusted_directory(directory) }
                .expect("create the compiled-runtime cache");
            Arc::new(cache)
        })
        .clone()
}

async fn prepare(
    preparation: &LocalPreparation,
    runtime: &[u8],
    files: Vec<PlatformFile>,
) -> Result<(BuildProduct, BuildRequest), PrepareError> {
    let request = request(runtime, files);
    let product = preparation
        .prepare(runtime, &request, &CancellationToken::new())
        .await?;
    Ok((product, request))
}

/// Prepares `files` on the shared runtime and answers `path` across a park
/// and a Store restoration, returning the response body.
async fn answer_across_restored_stores(files: Vec<PlatformFile>, path: &str) -> String {
    isoplane_harness::LogCapture::global();
    let runtime = isoplane_fixtures::mono_runtime();
    let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
    let (product, request) = prepare(&preparation, &runtime, files)
        .await
        .unwrap_or_else(|error| panic!("the package must prepare on the shared runtime: {error}"));
    let engine = IsoplaneEngine::new(request.engine.clone()).expect("the preparation engine");
    let identity = request.identity().expect("the preparation identity");
    // SAFETY: LocalPreparation in this process produced these bytes.
    let prepared = unsafe { engine.decode_builder_output(product.prepared_bytes(), identity) }
        .expect("the prepared template must decode");
    let reactor = no_egress::reactor_without_egress(7300);
    reactor.context.begin_slice(1, 1);
    let mut guest = {
        // SAFETY: This product was decoded from LocalPreparation output in this test module.
        unsafe { ReturnedGuest::prepared(prepared, &reactor) }
    };
    let first = guest.start(path);
    assert_eq!(
        first.completion,
        Completion::Parked,
        "the package's handler must park on its host timer: {first:?}; logs: {:?}",
        reactor.context.emitted_logs()
    );
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_millis(60)).await;
    reactor.context.begin_slice(61, 1);
    let resumed = guest.wake(&reactor).await;
    assert_eq!(
        resumed.completion,
        Completion::Done,
        "the package must finish after a Store restoration: {resumed:?}; logs: {:?}",
        reactor.context.emitted_logs()
    );
    reactor.response_text(1)
}

/// The detail of a preparation the shared runtime's loader refused.
async fn refusal(files: Vec<PlatformFile>) -> String {
    let runtime = isoplane_fixtures::mono_runtime();
    let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
    match prepare(&preparation, &runtime, files).await {
        Ok(_) => panic!("the loader must refuse the package"),
        Err(PrepareError::Rejected(failure)) => {
            assert_eq!(
                failure.stage,
                BuildStage::Initialization,
                "a package refusal must end initialization, not admission: {failure:?}"
            );
            failure.detail
        }
        Err(error) => panic!("the refusal must be the worker's structured one: {error}"),
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex digits"))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_smallest_package_runs_on_the_shared_runtime_across_restored_stores() {
    assert_eq!(
        answer_across_restored_stores(package("alpha"), "/greeting").await,
        "alpha:/greeting",
        "the package's own handler must answer through the shared runtime"
    );
}

#[tokio::test(start_paused = true)]
async fn a_package_with_its_own_dependencies_runs_on_the_shared_runtime_across_restored_stores() {
    let body = answer_across_restored_stores(package("beta"), "/beta").await;
    let report: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|error| panic!("{body} is JSON: {error}"));
    assert_eq!(
        report["Path"], "/beta",
        "the package must see its request: {body}"
    );
    assert_eq!(
        report["Sha256"], "53814b5ec17eef0a24410a69ea763ee282c07c34ebec0a994b40fed5cefdcdc1",
        "the BCL's native cryptography must hash the path: {body}"
    );
    assert_eq!(
        report["Total"], 14,
        "the package's project dependency must sum its generic value types: {body}"
    );
    let mut inflated = String::new();
    flate2::read::DeflateDecoder::new(&hex(report["Deflated"].as_str().expect("hex"))[..])
        .read_to_string(&mut inflated)
        .expect("the BCL's native compression must produce raw deflate");
    assert_eq!(
        inflated, "/beta",
        "compression must round-trip the path: {body}"
    );
    assert_eq!(
        report["Greeting"], "hallo",
        "the package's satellite assembly must supply its culture's resources: {body}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_packages_initialization_hook_runs_once_before_its_template_is_captured() {
    let body = answer_across_restored_stores(package("beta"), "/beta").await;
    let report: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|error| panic!("{body} is JSON: {error}"));
    assert_eq!(
        report["Warmups"], 1,
        "the hook the package's factory registers must warm the template exactly once: {body}"
    );
}

#[tokio::test]
async fn a_second_package_reuses_the_runtimes_native_compilation() {
    let runtime = isoplane_fixtures::mono_runtime();
    let directory = tempfile::tempdir().expect("an isolated compilation cache");
    // SAFETY: the cache creates this directory privately, empty, for this test,
    // and only CompilationCache's compiler path writes to it.
    let cache = unsafe { CompilationCache::from_trusted_directory(directory.path().join("cache")) }
        .expect("create the compilation cache");
    let preparation = LocalPreparation::with_cache(Arc::new(cache)).expect("local preparation");
    let (alpha, _) = prepare(&preparation, &runtime, package("alpha"))
        .await
        .expect("alpha must prepare");
    let (beta, _) = prepare(&preparation, &runtime, package("beta"))
        .await
        .expect("beta must prepare");
    assert!(
        alpha.initialized.compiled && !beta.initialized.compiled,
        "the first package compiles the runtime and the second must reuse it (first compiled: {}, second compiled: {})",
        alpha.initialized.compiled,
        beta.initialized.compiled
    );
}

#[tokio::test]
async fn an_empty_package_manifest_is_refused() {
    let detail = refusal(with_package_file(
        package("alpha"),
        "application.json",
        b"null",
    ))
    .await;
    assert!(
        detail.contains("PackageManifestException"),
        "an empty manifest must be refused as such: {detail}"
    );
}

#[tokio::test]
async fn a_package_assembly_the_runtime_supplies_is_refused() {
    let files = package("alpha");
    let impostor = package_file(&files, "assemblies/Alpha.dll");
    let detail = refusal(with_package_file(
        files,
        "assemblies/System.Text.Json.dll",
        &impostor,
    ))
    .await;
    assert!(
        detail.contains("PackageAssemblyConflictException"),
        "a package must not bring an assembly the runtime binds instead: {detail}"
    );
}

#[tokio::test]
async fn a_package_file_defining_another_assembly_is_refused() {
    let files = package("alpha");
    let impostor = package_file(&files, "assemblies/Alpha.dll");
    let detail = refusal(with_package_file(files, "assemblies/Gamma.dll", &impostor)).await;
    assert!(
        detail.contains("PackageAssemblyConflictException"),
        "a package file must hold the assembly it is named for: {detail}"
    );
}

#[tokio::test]
async fn a_factory_that_returns_no_handler_is_refused() {
    let files = package("alpha");
    let manifest = String::from_utf8(package_file(&files, "application.json"))
        .expect("the manifest is UTF-8")
        .replace("Alpha.Handler.Create", "Alpha.Handler.Describe");
    let detail = refusal(with_package_file(
        files,
        "application.json",
        manifest.as_bytes(),
    ))
    .await;
    assert!(
        detail.contains("PackageFactoryException"),
        "a factory must return a handler: {detail}"
    );
}

#[tokio::test]
async fn a_missing_factory_is_refused() {
    let files = package("alpha");
    let manifest = String::from_utf8(package_file(&files, "application.json"))
        .expect("the manifest is UTF-8")
        .replace("Alpha.Handler.Create", "Alpha.Handler.Missing");
    let detail = refusal(with_package_file(
        files,
        "application.json",
        manifest.as_bytes(),
    ))
    .await;
    assert!(
        detail.contains("PackageFactoryException"),
        "the declared factory must exist: {detail}"
    );
}

/// The `native` package's outcome for one transition, before and after a
/// Store restoration.
async fn native_use(path: &str) -> String {
    answer_across_restored_stores(package("native"), path).await
}

#[tokio::test(start_paused = true)]
async fn a_package_pinvoke_matching_a_runtime_declaration_is_called() {
    assert_eq!(
        native_use("/declared").await,
        "called/called",
        "a package may call a native entry point the runtime itself declares, with the same signature"
    );
}

#[tokio::test(start_paused = true)]
async fn a_package_pinvoke_with_a_mismatched_signature_is_refused() {
    assert_eq!(
        native_use("/mismatched").await,
        "EntryPointNotFoundException/EntryPointNotFoundException",
        "a declaration disagreeing with the native entry point's signature must be refused, not trap"
    );
}

#[tokio::test(start_paused = true)]
async fn a_package_pinvoke_with_a_signature_the_runtime_never_bridges_is_refused() {
    assert_eq!(
        native_use("/unbridged").await,
        "EntryPointNotFoundException/EntryPointNotFoundException",
        "a signature the runtime has no bridge for must be refused, not abort the guest"
    );
}

#[tokio::test(start_paused = true)]
async fn a_package_internal_call_is_refused() {
    assert_eq!(
        native_use("/internal-call").await,
        "MissingMethodException/MissingMethodException",
        "an internal call outside the runtime's own images must be refused, not abort the guest"
    );
}

#[tokio::test(start_paused = true)]
async fn a_package_unmanaged_callback_pointer_is_refused() {
    assert_eq!(
        native_use("/callback-pointer").await,
        "PlatformNotSupportedException/PlatformNotSupportedException",
        "a package's own [UnmanagedCallersOnly] method must not become a native pointer"
    );
}

#[tokio::test(start_paused = true)]
async fn a_package_delegate_marshalled_to_a_function_pointer_is_refused() {
    assert_eq!(
        native_use("/delegate-pointer").await,
        "PlatformNotSupportedException/PlatformNotSupportedException",
        "a package's delegate must not become a native pointer"
    );
}
