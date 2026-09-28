//! One Mono runtime module, linked and compiled with no application, runs
//! application packages it was never built with. Each package is served as
//! application files, loaded by the runtime's fixed loader during real
//! preparation, and executed through the reactor across a park and a
//! Store restoration.

use std::io::Read;
use std::sync::{Arc, OnceLock};

use isoplane_abi::config::{InitSandbox, ValidationLimits};
use isoplane_abi::filesystem::APPLICATION_PACKAGE_ROOT;
use isoplane_engine::prepared::PreparedTemplate;
use isoplane_engine::{CompilationCache, Completion, IsoplaneEngine, PlatformFile, PlatformFiles};
use isoplane_harness::EmittedGuestLogs as _;
use isoplane_harness::{engine_configuration, ReturnedGuest};
use isoplane_prepare::package::PackageLimits;
use isoplane_prepare::{
    BuildProduct, BuildRequest, BuildStage, Initialization, InitializationNetwork,
    LocalPreparation, PortableTemplate, Preparation, PrepareError, RuntimeProfile,
};
use tokio_util::sync::CancellationToken;

/// Limits used when reading package fixtures.
fn limits() -> PackageLimits {
    PackageLimits {
        bytes: isoplane_harness::GUEST_LIMITS.package_bytes,
        files: ValidationLimits::DEFAULT.max_package_files,
        manifest_bytes: ValidationLimits::DEFAULT.max_package_manifest_bytes,
    }
}

/// The package fixture `name` as the host serves it.
fn package(name: &str) -> Vec<PlatformFile> {
    let archive = std::fs::read(isoplane_fixtures::mono_package(name))
        .expect("the package fixture is readable");
    static RUNTIME_HASH: OnceLock<[u8; 32]> = OnceLock::new();
    let runtime_hash = RUNTIME_HASH.get_or_init(|| {
        use sha2::Digest;
        sha2::Sha256::digest(isoplane_fixtures::mono_runtime()).into()
    });
    let package = isoplane_prepare::package::read(&archive, limits())
        .unwrap_or_else(|error| panic!("the {name} package must map to served files: {error}"));
    assert_eq!(
        package.runtime.module_sha256, *runtime_hash,
        "the {name} fixture must name the source-built runtime under test"
    );
    package.files.files().to_vec()
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

fn network() -> InitializationNetwork {
    let host = isoplane_host::CellConfig::default();
    InitializationNetwork {
        tenant: "returned-guest-test".into(),
        allowlist: None,
        allowed_destinations: Vec::new(),
        limits: Default::default(),
        native_handles: host.guest_resources.native_handles,
        pending_futures: isoplane_harness::GUEST_LIMITS
            .limits()
            .pending_futures_per_dispatch,
    }
}

fn request(runtime: &[u8], files: Vec<PlatformFile>) -> BuildRequest {
    BuildRequest {
        engine: engine_configuration(PlatformFiles::default()),
        application_files: PlatformFiles::new(files)
            .expect("package paths are valid application files"),
        runtime: RuntimeProfile::raw_wasm(),
        limits: isoplane_harness::GUEST_LIMITS,
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        initialization: Initialization {
            network: Some(network()),
            runtime_config: b"{}".to_vec(),
            hash_seed: [7; 16],
            config: b"{}".to_vec(),
            sandbox: InitSandbox::RuntimeBringUp,
            cpu_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_CPU_MS,
            wall_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
            max_passes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_PASSES,
            log_bytes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
        },
        input_hash: *blake3::hash(runtime).as_bytes(),
        portable_template: PortableTemplate::Omitted,
    }
}

/// Compilations of the runtime shared by every preparation in this binary
/// that is not itself about compilation reuse.
fn shared_cache() -> Arc<CompilationCache> {
    static CACHE: OnceLock<Arc<CompilationCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let storage = isoplane_cache::Cache::new(
                isoplane_fixtures::target_directory().join("compiled-tests"),
                isoplane_cache::PROJECT_CACHE_BYTES,
            )
            .expect("open bounded runtime test cache");
            let binary = std::fs::File::open(std::env::current_exe().expect("test binary path"))
                .expect("open test binary identity");
            let mut identity =
                blake3::Hasher::new_derive_key("isoplane runtime acceptance compiled cache");
            identity
                .update_reader(binary)
                .expect("hash test binary identity");
            let lease = storage
                .lease(isoplane_cache::CacheKey(*identity.finalize().as_bytes()))
                .expect("lease runtime test cache");
            let directory = lease.directory().to_path_buf();
            static LEASE: OnceLock<isoplane_cache::Lease> = OnceLock::new();
            assert!(
                LEASE.set(lease).is_ok(),
                "the shared test cache must acquire one process lifetime lease"
            );
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

/// `product`, prepared for `request` in this process, as a cell receives it.
fn decode(product: &BuildProduct, request: &BuildRequest) -> PreparedTemplate {
    let engine = IsoplaneEngine::new(request.engine.clone()).expect("the preparation engine");
    let identity = request.identity().expect("the preparation identity");
    // SAFETY: LocalPreparation in this process produced these bytes.
    unsafe { engine.decode_builder_output(product.prepared_bytes(), identity) }
        .expect("the prepared template must decode")
}

/// Prepares `files` on the shared runtime and answers `path` across a park
/// on its host timer and a Store restoration at every later park, returning
/// the response body.
async fn answer_across_restored_stores(files: Vec<PlatformFile>, path: &str) -> String {
    let runtime = isoplane_fixtures::mono_runtime();
    let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
    let (product, request) = prepare(&preparation, &runtime, files)
        .await
        .unwrap_or_else(|error| panic!("the package must prepare on the shared runtime: {error}"));
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let guest = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { ReturnedGuest::prepared(decode(&product, &request), &reactor) }
    };
    finish_across_restored_stores(guest, &reactor, path).await
}

/// Answers `path` on `guest` across a park on its host timer and a
/// Store restoration at every later park, returning the response body.
async fn finish_across_restored_stores(
    mut guest: ReturnedGuest,
    reactor: &isoplane_harness::Reactor,
    path: &str,
) -> String {
    isoplane_harness::LogCapture::global();
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
    guest.finish(reactor, 8).await
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

#[tokio::test(start_paused = true)]
async fn an_aspnet_package_runs_on_the_shared_runtime_across_restored_stores() {
    let body = answer_across_restored_stores(package("web"), "/hello/isoplane").await;
    assert!(
        body.starts_with("hello, isoplane;"),
        "the package's ASP.NET route must answer from its content file through the runtime's adapter: {body}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_packages_warmup_runs_once_before_its_template_is_captured() {
    let body = answer_across_restored_stores(package("web"), "/hello/isoplane").await;
    assert!(
        body.ends_with("; warmups=1"),
        "the warm-up the package's factory registers must reach its pipeline exactly once: {body}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_package_warms_its_read_route_before_template_capture() {
    let body = answer_across_restored_stores(package("web"), "/hello/isoplane").await;
    assert!(
        body.contains("; greetings=2;"),
        "the first real greeting must follow one completed initialization greeting: {body}"
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

#[tokio::test(start_paused = true)]
async fn an_unused_package_assembly_loads_on_demand_and_survives_store_restoration() {
    let library = package_file(&package("beta"), "assemblies/Beta.Library.dll");
    let files = with_package_file(package("alpha"), "assemblies/Beta.Library.dll", &library);
    let body = answer_across_restored_stores(files, "/load-library").await;
    assert_eq!(
        body,
        "initialized=False;loaded=Beta.Library",
        "an unused dependency must remain unloaded during preparation and load successfully before a parked request resumes"
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
        detail.contains("PackageAssemblyConflictException")
            && detail.contains("supplied by the runtime"),
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

/// The first dispatch of `path` on the prepared `diagnostics` package, and
/// what the guest logged.
async fn diagnose(path: &str) -> (isoplane_engine::DispatchOutcome, Vec<String>) {
    isoplane_harness::LogCapture::global();
    let runtime = isoplane_fixtures::mono_runtime();
    let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
    let (product, request) = prepare(&preparation, &runtime, package("diagnostics"))
        .await
        .unwrap_or_else(|error| panic!("the diagnostics package must prepare: {error}"));
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let mut guest = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { ReturnedGuest::prepared(decode(&product, &request), &reactor) }
    };
    let outcome = guest.start(path);
    let logs = reactor
        .context
        .emitted_logs()
        .into_iter()
        .map(|record| record.fields)
        .collect();
    (outcome, logs)
}

#[tokio::test]
async fn a_runtime_fail_fast_logs_its_reason_before_ending_the_instance() {
    let (outcome, logs) = diagnose("/fail-fast").await;
    assert_eq!(
        outcome.completion,
        Completion::Trapped,
        "a fail-fast must end the instance: {outcome:?}"
    );
    let line = |text: &str| logs.iter().position(|line| line.contains(text));
    let (Some(terminated), Some(reason)) = (
        line("Process terminated."),
        line("the diagnostics package gave up"),
    ) else {
        panic!("the runtime's fail-fast warnings must reach the guest log: {logs:?}; {outcome:?}");
    };
    assert!(
        terminated < reason,
        "a runtime warning must not end the instance before the next one is logged: {logs:?}"
    );
}

#[tokio::test]
async fn a_fatal_runtime_error_ends_the_instance_with_its_message() {
    let (outcome, logs) = diagnose("/invalid-il").await;
    let trap = outcome
        .trap
        .as_ref()
        .unwrap_or_else(|| panic!("a fatal runtime error must end the instance: {outcome:?}"));
    // The runtime is built without fatal message text; its report names the
    // source location instead.
    assert!(
        trap.message.contains("interp/transform.c:"),
        "the instance must end with the runtime's own report, not a console write: {trap:?}; logs: {logs:?}"
    );
}

/// The debug runtime's first dispatch under the debug session file `agent`,
/// or none, with the alpha package served.
fn debug_boot(agent: Option<&str>) -> isoplane_engine::DispatchOutcome {
    let mut files = package("alpha");
    if let Some(agent) = agent {
        files.push(PlatformFile {
            path: isoplane_abi::filesystem::DEBUG_AGENT_PATH.into(),
            bytes: agent.as_bytes().into(),
            modified_ns: 0,
        });
    }
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new(&isoplane_fixtures::mono_debug_runtime(), &reactor);
    guest.serve_platform_files(PlatformFiles::new(files).expect("valid debug session files"));
    guest.start("/greeting")
}

fn refusal_message(outcome: &isoplane_engine::DispatchOutcome) -> String {
    let trap = outcome
        .trap
        .as_ref()
        .unwrap_or_else(|| panic!("the debug runtime must refuse to boot: {outcome:?}"));
    assert_eq!(
        trap.kind,
        isoplane_engine::TrapKind::GuestPanic,
        "the refusal must be the runtime's own abort: {trap:?}"
    );
    trap.message.clone()
}

#[tokio::test]
async fn a_debug_runtime_refuses_a_session_naming_anything_but_a_loopback_port() {
    for agent in [
        "127.0.0.1:0",
        "127.0.0.1:65536",
        "127.0.0.1:42,server=y",
        "192.168.0.1:1234",
        "127.0.0.1:",
    ] {
        let message = refusal_message(&debug_boot(Some(agent)));
        assert!(
            message.contains("the debug session must name"),
            "a session file must select only a loopback port, never other agent options ({agent}): {message}"
        );
    }
}

#[tokio::test]
async fn a_debug_runtime_without_a_debug_session_is_refused() {
    let message = refusal_message(&debug_boot(None));
    assert!(
        message.contains("requires the debug session"),
        "the debug runtime must run only where a local debug session serves its debugger: {message}"
    );
}

/// Packages prepared as layers over the shared runtime's baseline, which
/// only Linux maps.
#[cfg(target_os = "linux")]
mod over_the_runtime_baseline {
    use super::*;
    use isoplane_prepare::{PreparedBaseline, RuntimeBaseline};

    #[test]
    fn a_runtime_that_runs_out_of_memory_in_start_up_reports_its_own_diagnosis() {
        let runtime = isoplane_fixtures::mono_runtime();
        let mut request = request(&runtime, Vec::new());
        // The runtime's declared initial memory fits; its start-up does not.
        request.runtime = RuntimeProfile::SharedRuntime {
            sdk_version: "0.0.0-test".into(),
        };
        request.guest_memory_bytes =
            Some(isoplane_abi::config::GuestMemoryBytes::new(64 << 20).expect("a page multiple"));
        let failure =
            RuntimeBaseline::prepare(&runtime, &request.baseline(), Some(&shared_cache()))
                .err()
                .expect("the runtime's start-up must not fit in 64 MiB");
        let detail = format!("{failure:?}");
        assert!(
            detail.contains("guest aborted: Garbage collector could not allocate"),
            "an operator must see the runtime's own diagnosis of a failed start-up: {detail}"
        );
    }

    /// The shared runtime prepared once to the end of its own initializer, for
    /// packages prepared with [`request`].
    fn runtime_baseline(runtime: &[u8]) -> PreparedBaseline {
        RuntimeBaseline::prepare(
            runtime,
            &request(runtime, Vec::new()).baseline(),
            Some(&shared_cache()),
        )
        .unwrap_or_else(|failure| {
            panic!("the shared runtime must prepare as a baseline: {failure:?}")
        })
    }

    /// `files` prepared as a layer over `baseline`, with the baseline, both as a
    /// cell receives them.
    async fn layer_over(
        baseline: &PreparedBaseline,
        runtime: &[u8],
        files: Vec<PlatformFile>,
    ) -> (PreparedTemplate, PreparedTemplate) {
        let request = request(runtime, files);
        let source = baseline.baseline.clone();
        let (layer, request) = tokio::task::spawn_blocking(move || {
            let layer = source
                .prepare_application(&request)
                .unwrap_or_else(|failure| {
                    panic!("the package must prepare over the baseline: {failure:?}")
                })
                .product;
            (layer, request)
        })
        .await
        .expect("the preparation worker must finish");
        let engine = IsoplaneEngine::new(request.engine.clone()).expect("the preparation engine");
        let base_identity = request
            .baseline()
            .identity()
            .expect("the baseline's identity");
        // SAFETY: LocalPreparation in this process produced these bytes.
        let base = unsafe {
            engine.decode_builder_output(baseline.product.prepared_bytes(), base_identity)
        }
        .expect("the baseline must decode");
        let loaded = {
            // SAFETY: This product was decoded from LocalPreparation output in this test module.
            unsafe { engine.load_prepared_product(&base) }
        }
        .expect("the baseline must load");
        let identity = request.identity().expect("the layer's identity");
        // SAFETY: LocalPreparation in this process produced these bytes.
        let layer =
            unsafe { engine.decode_builder_output_over(&loaded, layer.prepared_bytes(), identity) }
                .expect("the layer must decode over its baseline");
        (base, layer)
    }

    #[tokio::test(start_paused = true)]
    async fn whole_and_layered_preparation_preserve_the_same_initialized_application_state() {
        let runtime = isoplane_fixtures::mono_runtime();
        let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
        let (whole, request) = prepare(&preparation, &runtime, package("beta"))
            .await
            .expect("beta must prepare whole");
        let baseline = runtime_baseline(&runtime);
        let (base, layer) = layer_over(&baseline, &runtime, package("beta")).await;
        let whole_reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        whole_reactor.context.begin_slice(1, 1);
        let whole = finish_across_restored_stores(
            {
                // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
                unsafe { ReturnedGuest::prepared(decode(&whole, &request), &whole_reactor) }
            },
            &whole_reactor,
            "/beta",
        )
        .await;
        let layered_reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        layered_reactor.context.begin_slice(1, 1);
        let layered = finish_across_restored_stores(
            {
                // SAFETY: LocalPreparation/RuntimeBaseline produced both products through the local layer_over helper.
                unsafe { ReturnedGuest::layered(base, layer, &layered_reactor) }
            },
            &layered_reactor,
            "/beta",
        )
        .await;
        assert_eq!(
            layered, whole,
            "layered and whole preparation must preserve the same application response across Store restoration"
        );
        let report: serde_json::Value = serde_json::from_str(&layered)
            .expect("the captured application must return its JSON report");
        assert_eq!(
            report["Warmups"], 1,
            "both preparation paths must retain exactly one completed initialization warmup"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn packages_over_one_runtime_baseline_each_answer_across_restored_stores() {
        let runtime = isoplane_fixtures::mono_runtime();
        let baseline = runtime_baseline(&runtime);
        let (alpha_base, alpha) = layer_over(&baseline, &runtime, package("alpha")).await;
        let (beta_base, beta) = layer_over(&baseline, &runtime, package("beta")).await;
        let alpha_reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        alpha_reactor.context.begin_slice(1, 1);
        let alpha = finish_across_restored_stores(
            {
                // SAFETY: LocalPreparation/RuntimeBaseline produced both products through the local layer_over helper.
                unsafe { ReturnedGuest::layered(alpha_base, alpha, &alpha_reactor) }
            },
            &alpha_reactor,
            "/greeting",
        )
        .await;
        let beta_reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        beta_reactor.context.begin_slice(1, 1);
        let beta = finish_across_restored_stores(
            {
                // SAFETY: LocalPreparation/RuntimeBaseline produced both products through the local layer_over helper.
                unsafe { ReturnedGuest::layered(beta_base, beta, &beta_reactor) }
            },
            &beta_reactor,
            "/beta",
        )
        .await;
        assert_eq!(
            alpha, "alpha:/greeting",
            "alpha's layer must answer with its own handler over the shared baseline"
        );
        assert!(
            beta.contains("\"Path\":\"/beta\""),
            "beta's layer must answer with its own handler over the same baseline: {beta}"
        );
    }
}
