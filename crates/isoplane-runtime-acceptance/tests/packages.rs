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

#[path = "common/admitted.rs"]
pub mod admitted;
#[path = "common/debugger_transport.rs"]
mod debugger_transport;
#[path = "common/fixed_dns.rs"]
mod fixed_dns;
#[path = "common/http_peers.rs"]
pub mod http_peers;
#[path = "common/tls_chains.rs"]
pub mod tls_chains;
#[path = "common/tls_material.rs"]
pub mod tls_material;

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
        destinations: isoplane_prepare::InitializationDestinations::Public {
            exceptions: Vec::new(),
        },
        limits: Default::default(),
        native_handles: host.guest_resources.native_handles,
        pending_futures: isoplane_harness::GUEST_LIMITS
            .limits()
            .pending_futures_per_dispatch,
        buffered_bytes: host.buffered_future_bytes,
    }
}

fn request(runtime: &[u8], files: Vec<PlatformFile>) -> BuildRequest {
    BuildRequest {
        engine: engine_configuration(PlatformFiles::default()),
        application_files: PlatformFiles::new(files)
            .expect("package paths are valid application files"),
        runtime: RuntimeProfile::SharedRuntime {
            sdk_version: isoplane_fixtures::release_version(),
        },
        limits: isoplane_harness::GUEST_LIMITS,
        guest_memory_bytes: None,
        validation_limits: Default::default(),
        initialization: Initialization {
            network: Some(network()),
            runtime_config: b"{}".to_vec(),
            hash_seed: [7; 16],
            config: b"{}".to_vec(),
            sandbox: InitSandbox::RuntimeBringUp,
            occupancy_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_OCCUPANCY_MS,
            wall_ms: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_WALL_MS,
            max_passes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_PASSES,
            log_bytes: isoplane_prepare::preinit::DEFAULT_INITIALIZATION_LOG_BYTES,
        },
        input_hash: *blake3::hash(runtime).as_bytes(),
        portable_template: PortableTemplate::Omitted,
    }
}

/// The runtime Task compiled for the engine every preparation in this binary
/// uses, except those about compilation reuse.
fn shared_cache() -> Arc<CompilationCache> {
    isoplane_fixtures::compiled_runtime_cache()
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
    let (product, request) = prepare_on_the_shared_runtime(files).await;
    answer_on_an_instance(&product, &request, isoplane_host::InstanceId::alloc(), path).await
}

async fn prepare_on_the_shared_runtime(files: Vec<PlatformFile>) -> (BuildProduct, BuildRequest) {
    let runtime = isoplane_fixtures::mono_runtime();
    let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
    prepare(&preparation, &runtime, files)
        .await
        .unwrap_or_else(|error| panic!("the package must prepare on the shared runtime: {error}"))
}

/// `name`, prepared on the shared runtime once for every test in this binary that only asks
/// it for a route; a preparation is the template every instance starts from, so a test
/// that activates its own instance cannot see another's requests.
async fn prepared_package(name: &'static str) -> Arc<(BuildProduct, BuildRequest)> {
    type Prepared = Arc<tokio::sync::OnceCell<Arc<(BuildProduct, BuildRequest)>>>;
    static PACKAGES: OnceLock<std::sync::Mutex<std::collections::HashMap<&'static str, Prepared>>> =
        OnceLock::new();
    let cell = PACKAGES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(name)
        .or_default()
        .clone();
    cell.get_or_init(|| async { Arc::new(prepare_on_the_shared_runtime(package(name)).await) })
        .await
        .clone()
}

/// Answers `path` on a fresh instance of the shared preparation of package `name`, as
/// [`answer_across_restored_stores`] does.
async fn answer_package(name: &'static str, path: &str) -> String {
    let prepared = prepared_package(name).await;
    answer_on_an_instance(
        &prepared.0,
        &prepared.1,
        isoplane_host::InstanceId::alloc(),
        path,
    )
    .await
}

/// Activates `instance` from the prepared template and answers `path` on it
/// as [`answer_across_restored_stores`] does.
async fn answer_on_an_instance(
    product: &BuildProduct,
    request: &BuildRequest,
    instance: isoplane_host::InstanceId,
    path: &str,
) -> String {
    let reactor = isoplane_harness::Reactor::new(instance);
    reactor.context.begin_slice(1, 1);
    let guest = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { ReturnedGuest::prepared(decode(product, request), &reactor) }
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

/// The detail of a preparation whose initialization on the shared runtime was refused.
async fn refusal(files: Vec<PlatformFile>) -> String {
    let runtime = isoplane_fixtures::mono_runtime();
    let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
    match prepare(&preparation, &runtime, files).await {
        Ok(_) => panic!("preparation must refuse the package"),
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
        answer_package("alpha", "/greeting").await,
        "alpha:/greeting",
        "the package's own handler must answer through the shared runtime"
    );
}

#[tokio::test(start_paused = true)]
async fn a_package_with_its_own_dependencies_runs_on_the_shared_runtime_across_restored_stores() {
    let body = answer_package("beta", "/beta").await;
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
    let body = answer_package("beta", "/beta").await;
    let report: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|error| panic!("{body} is JSON: {error}"));
    assert_eq!(
        report["Warmups"], 1,
        "the package's initializer must warm the template exactly once: {body}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_package_answers_across_restored_stores_after_one_warmup() {
    let body = answer_package("web", "/hello/isoplane").await;
    assert!(
        body.starts_with("hello, isoplane;"),
        "the package's ASP.NET route must answer from its content file through the runtime's adapter: {body}"
    );
    assert!(
        body.ends_with("; warmups=1"),
        "the warm-up the package's entry point subscribes must reach its pipeline exactly once: {body}"
    );
    assert!(
        body.contains("; greetings=2;"),
        "the first real greeting must follow one completed initialization greeting: {body}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_package_serves_its_static_web_assets() {
    let body = answer_package("web", "/site.css").await;
    assert_eq!(
        body, "body { color: teal; }\n",
        "a file under the package's wwwroot must be served from the package"
    );
}

/// The application log the web package's `/log` route leaves on an instance,
/// with its `ConsoleLogging` setting naming the logging calls its builder
/// makes: the level and text of each record of the route's category.
async fn logged_with(calls: &str) -> Vec<(u8, String)> {
    isoplane_harness::LogCapture::global();
    let runtime = isoplane_fixtures::mono_runtime();
    let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
    let mut request = request(&runtime, package("web"));
    request.initialization.runtime_config =
        serde_json::to_vec(&serde_json::json!({ "ConsoleLogging": calls }))
            .expect("the settings serialize");
    let product = preparation
        .prepare(&runtime, &request, &CancellationToken::new())
        .await
        .unwrap_or_else(|error| panic!("the web package calling {calls} must prepare: {error}"));
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let guest = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { ReturnedGuest::prepared(decode(&product, &request), &reactor) }
    };
    let body = finish_across_restored_stores(guest, &reactor, "/log").await;
    assert_eq!(
        body,
        "logged",
        "the route must answer after logging; logs: {:?}",
        reactor.context.emitted_logs()
    );
    reactor
        .context
        .emitted_logs()
        .into_iter()
        .filter(|record| record.fields.contains("Web.Orders"))
        .map(|record| (record.level, record.fields))
        .collect()
}

const INFORMATION: u8 = 2;
const ERROR: u8 = 4;

#[tokio::test(start_paused = true)]
async fn an_aspnet_packages_console_logger_writes_each_entry_once_in_place_of_isoplanes_record() {
    assert_eq!(
        logged_with("AddConsole").await,
        [(INFORMATION, "warn: Web.Orders[3]\n      late by 5".to_owned())],
        "AddConsole must start without a thread and write each entry once, as the simple formatter formats it, to standard output"
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_package_that_clears_the_providers_and_adds_the_console_logger_logs_each_entry_once(
) {
    assert_eq!(
        logged_with("ClearProviders AddConsole").await,
        [(
            INFORMATION,
            "warn: Web.Orders[3]\n      late by 5".to_owned()
        )],
        "ClearProviders followed by AddConsole must leave the console logger as the only one"
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_package_that_only_clears_the_providers_logs_nothing() {
    assert_eq!(
        logged_with("ClearProviders").await,
        [],
        "ClearProviders must remove Isoplane's provider as it removes any other"
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_packages_json_console_logger_writes_with_its_formatter_options() {
    let logged = logged_with("AddJsonConsole").await;
    let [(level, fields)] = &logged[..] else {
        panic!("AddJsonConsole must write the entry once: {logged:?}");
    };
    assert_eq!(*level, INFORMATION, "the entry goes to standard output");
    assert!(
        fields.starts_with("{\n  \"EventId\": 3,"),
        "the formatter must write with the indented writer options: {fields}"
    );
    let entry: serde_json::Value = serde_json::from_str(fields)
        .unwrap_or_else(|error| panic!("the entry is JSON: {error}: {fields}"));
    assert_eq!(
        (
            &entry["Category"],
            &entry["Message"],
            &entry["Scopes"][0]["RequestPath"],
            &entry["Scopes"][1]["Id"]
        ),
        (
            &serde_json::json!("Web.Orders"),
            &serde_json::json!("late by 5"),
            &serde_json::json!("/log"),
            &serde_json::json!(42)
        ),
        "the formatter must write the category, message, and the request's and route's scopes it was configured to include: {fields}"
    );
}

/// `logged` as one record at `level` whose text is `prefix`, ASP.NET's
/// request identifier, then `suffix`.
fn assert_one_entry_around_the_request_id(
    logged: &[(u8, String)],
    level: u8,
    prefix: &str,
    suffix: &str,
) {
    let [(actual, fields)] = logged else {
        panic!("the entry must be written once: {logged:?}");
    };
    assert_eq!(*actual, level, "the entry's log level: {fields}");
    assert!(
        fields.starts_with(prefix) && fields.ends_with(suffix),
        "the entry must read {prefix}<request id>{suffix}: {fields}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_packages_simple_console_logger_writes_with_its_formatter_options() {
    // Single-line, with scopes and a literal timestamp, and no colour in the sandbox.
    assert_one_entry_around_the_request_id(
        &logged_with("AddSimpleConsole").await,
        INFORMATION,
        "at warn: Web.Orders[3] => RequestPath:/log RequestId:",
        " => order 42 late by 5",
    );
}

#[tokio::test(start_paused = true)]
async fn an_aspnet_packages_systemd_console_logger_sends_entries_over_the_threshold_to_standard_error(
) {
    assert_one_entry_around_the_request_id(
        &logged_with("AddSystemdConsole").await,
        ERROR,
        "<4>Web.Orders[3] => RequestPath:/log RequestId:",
        " => order 42 late by 5",
    );
}

/// The web package prepared with its `Peer` setting naming `peer`, which its
/// initialization reaches as a local development session's does.
fn calling_request(runtime: &[u8], peer: std::net::SocketAddr) -> BuildRequest {
    let mut request = request(runtime, package("web"));
    request.initialization.runtime_config =
        serde_json::to_vec(&serde_json::json!({ "Peer": format!("http://{peer}") }))
            .expect("the settings serialize");
    if let Some(network) = request.initialization.network.as_mut() {
        network.destinations = isoplane_prepare::InitializationDestinations::LocalDevelopment;
    }
    request
}

#[tokio::test]
async fn an_aspnet_request_that_calls_out_through_a_factory_client_leaves_its_instance_reusable() {
    let peer = http_peers::http_peer().await;
    let runtime = isoplane_fixtures::mono_runtime();
    let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
    let request = calling_request(&runtime, peer.address);
    let product = preparation
        .prepare(&runtime, &request, &CancellationToken::new())
        .await
        .unwrap_or_else(|error| {
            panic!("a route that calls out through a factory client must warm up: {error}")
        });
    let warmed = peer.connections();
    let reactor = isoplane_harness::Reactor::with_network(
        isoplane_host::InstanceId::alloc(),
        admitted::admitting(
            vec![peer.address],
            Box::new(fixed_dns::FixedDns(vec![peer.address.ip()])),
        ),
    );
    let mut guest = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { ReturnedGuest::prepared(decode(&product, &request), &reactor) }
    };
    for dispatch in 1..=2u64 {
        reactor.context.begin_dispatch(1, dispatch);
        let body = guest.answer(&reactor, "/outbound", 32).await;
        assert_eq!(
            body,
            format!("hello from peer|hello from peer; calls={}", dispatch + 1),
            "request {dispatch} must follow the warm-up on the same application state"
        );
        assert!(
            !reactor
                .futures
                .end_dispatch(reactor.context.instance, dispatch),
            "request {dispatch} must leave no read pending, or the cell discards its instance"
        );
        // Closing a socket aborts the reactor tasks waiting on it, which let
        // go of the connection when the reactor next runs.
        let egress = reactor.runtime().services.egress.clone();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while egress.instance_connections(reactor.context.instance) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("request {dispatch} must close every connection its calls opened")
        });
    }
    assert_eq!(
        (warmed, peer.connections() - warmed),
        (2, 4),
        "every call must open its own connection rather than keep one idle"
    );
}

/// One browser's visits to the `session` package, prepared with `settings`,
/// on one instance: each visit sends the cookies earlier responses set.
struct SessionVisitor {
    guest: ReturnedGuest,
    reactor: isoplane_harness::Reactor,
    cookies: Vec<(String, String)>,
    dispatch: u64,
    now_ms: u64,
}

/// What one visit answered, and whether it left a timer or read pending,
/// which would cost the instance in a cell.
struct Visit {
    status: u16,
    location: Option<String>,
    body: String,
    left_work_pending: bool,
}

impl SessionVisitor {
    async fn new(settings: serde_json::Value) -> Self {
        let runtime = isoplane_fixtures::mono_runtime();
        let preparation = LocalPreparation::with_cache(shared_cache()).expect("local preparation");
        let mut request = request(&runtime, package("session"));
        request.initialization.runtime_config =
            serde_json::to_vec(&settings).expect("the settings serialize");
        let product = preparation
            .prepare(&runtime, &request, &CancellationToken::new())
            .await
            .unwrap_or_else(|error| panic!("the session package must prepare: {error}"));
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        let guest = {
            // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
            unsafe { ReturnedGuest::prepared(decode(&product, &request), &reactor) }
        };
        Self {
            guest,
            reactor,
            cookies: Vec::new(),
            dispatch: 0,
            now_ms: 1,
        }
    }

    /// Visits `path`, failing unless the dispatch completes.
    async fn visit(&mut self, method: &str, path: &str) -> Visit {
        let outcome = self.dispatch(method, path).await;
        let response = self.guest.response_of_outcome(outcome);
        let header = |wanted: &'static str| {
            response
                .head
                .headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(wanted))
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>()
        };
        for set in header("set-cookie") {
            let pair = set.split(';').next().unwrap_or_default();
            let (name, value) = pair
                .split_once('=')
                .expect("a cookie is a name and a value");
            self.cookies.retain(|(kept, _)| kept != name);
            self.cookies.push((name.to_owned(), value.to_owned()));
        }
        Visit {
            status: response.head.status,
            location: header("location").into_iter().next(),
            body: String::from_utf8_lossy(&response.body()).into_owned(),
            left_work_pending: self
                .reactor
                .futures
                .end_dispatch(self.reactor.context.instance, self.dispatch),
        }
    }

    /// Dispatches `method path` with the visitor's cookies until it stops parking.
    async fn dispatch(&mut self, method: &str, path: &str) -> isoplane_engine::DispatchOutcome {
        self.dispatch += 1;
        self.reactor
            .context
            .begin_dispatch(self.now_ms, self.dispatch);
        let cookie = self
            .cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        let headers = if cookie.is_empty() {
            vec![]
        } else {
            vec![("Cookie".to_owned(), cookie)]
        };
        let mut outcome = self
            .guest
            .start_request(isoplane_harness::request(method, path, headers));
        let mut wakes = 0;
        while outcome.completion == Completion::Parked {
            assert!(
                wakes < 32,
                "{method} {path} must finish within 32 wakes; logs: {:?}",
                self.logs()
            );
            // Each park waits on one of the cache's one-millisecond timers,
            // which falls due by the clock a cell sets as a slice begins.
            tokio::time::advance(std::time::Duration::from_millis(1)).await;
            self.now_ms += 1;
            self.reactor.context.begin_slice(self.now_ms, self.dispatch);
            outcome = self.guest.wake(&self.reactor).await;
            wakes += 1;
        }
        outcome
    }

    fn logs(&self) -> Vec<String> {
        isoplane_harness::LogCapture::global();
        self.reactor
            .context
            .emitted_logs()
            .into_iter()
            .map(|record| record.fields)
            .collect()
    }
}

#[tokio::test(start_paused = true)]
async fn a_session_value_written_over_an_asynchronous_cache_is_read_by_the_next_request() {
    let mut visitor = SessionVisitor::new(serde_json::json!({})).await;
    for expected in ["1", "2", "3"] {
        let visit = visitor.visit("GET", "/visits").await;
        assert_eq!(
            (visit.status, visit.body.as_str()),
            (200, expected),
            "each visit must read the count the one before it stored; logs: {:?}",
            visitor.logs()
        );
        assert!(
            !visit.left_work_pending,
            "loading and saving the session must leave no timer or read pending"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn tempdata_kept_in_a_session_over_an_asynchronous_cache_survives_a_redirect() {
    let mut visitor = SessionVisitor::new(serde_json::json!({})).await;
    let saved = visitor.visit("POST", "/notes").await;
    assert_eq!(
        (saved.status, saved.location.as_deref()),
        (302, Some("/notes")),
        "saving must redirect; logs: {:?}",
        visitor.logs()
    );
    let shown = visitor.visit("GET", "/notes").await;
    assert_eq!(
        (shown.status, shown.body.as_str()),
        (200, "note saved"),
        "the page redirected to must show the message saving left; logs: {:?}",
        visitor.logs()
    );
}

#[tokio::test(start_paused = true)]
async fn a_session_over_the_in_memory_cache_works_without_loading_it_first() {
    let mut visitor =
        SessionVisitor::new(serde_json::json!({ "Cache": "memory", "Pipeline": "UseSession" }))
            .await;
    for expected in ["1", "2"] {
        let visit = visitor.visit("GET", "/visits").await;
        assert_eq!(
            (visit.status, visit.body.as_str()),
            (200, expected),
            "the in-memory cache loads synchronously, so UseSession alone must keep the count; logs: {:?}",
            visitor.logs()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_session_that_could_not_be_loaded_fails_the_request_naming_use_loaded_session() {
    let mut visitor = SessionVisitor::new(serde_json::json!({ "Pipeline": "UseSession" })).await;
    let outcome = visitor.dispatch("GET", "/visits").await;
    let logs = visitor.logs();
    assert_eq!(
        outcome.completion,
        Completion::Trapped,
        "using a session the asynchronous cache could not load synchronously must fail the request, \
         not carry on with an empty one: {outcome:?}; logs: {logs:?}"
    );
    assert!(
        logs.iter().any(|log| log.starts_with(
            "unhandled System.InvalidOperationException: The session could not be loaded"
        ) && log.contains("Call app.UseLoadedSession()")),
        "the failure must say what to call; logs: {logs:?}"
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
async fn a_recorded_handler_of_another_shape_is_refused() {
    let files = package("alpha");
    let manifest = String::from_utf8(package_file(&files, "application.json"))
        .expect("the manifest is UTF-8")
        .replace("\"HandleAsync\"", "\"Describe\"");
    assert!(
        manifest.contains("\"Describe\""),
        "the manifest must name its handler method for this test to replace: {manifest}"
    );
    let detail = refusal(with_package_file(
        files,
        "application.json",
        manifest.as_bytes(),
    ))
    .await;
    assert!(
        detail.contains("PackageEntryException"),
        "the recorded handler must take a request and return a response: {detail}"
    );
}

#[tokio::test]
async fn a_recorded_handler_the_assembly_lacks_is_refused() {
    let files = package("alpha");
    let manifest = String::from_utf8(package_file(&files, "application.json"))
        .expect("the manifest is UTF-8")
        .replace("\"HandleAsync\"", "\"Missing\"");
    assert!(
        manifest.contains("\"Missing\""),
        "the manifest must name its handler method for this test to replace: {manifest}"
    );
    let detail = refusal(with_package_file(
        files,
        "application.json",
        manifest.as_bytes(),
    ))
    .await;
    assert!(
        detail.contains("PackageEntryException"),
        "the recorded handler must exist: {detail}"
    );
}

/// Answers `path` on `guest` at `at_ms` on its clock, across a park on a
/// handler's own 50 ms delay if it takes one, and ends the request as the cell
/// does.
async fn answer_at(
    guest: &mut ReturnedGuest,
    reactor: &isoplane_harness::Reactor,
    path: &str,
    at_ms: u64,
    dispatch: u64,
) -> String {
    reactor.context.begin_slice(at_ms, dispatch);
    let started = guest.start(path);
    let body = if started.completion == Completion::Parked {
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(60)).await;
        reactor.context.begin_slice(at_ms + 61, dispatch);
        guest.finish(reactor, 8).await
    } else {
        guest.text_of_outcome(started)
    };
    reactor
        .futures
        .end_dispatch(reactor.context.instance, dispatch);
    body
}

#[tokio::test(start_paused = true)]
async fn the_rate_limiter_middleware_starts_and_limits_an_instances_requests() {
    let prepared = prepared_package("web").await;
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let mut guest = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { ReturnedGuest::prepared(decode(&prepared.0, &prepared.1), &reactor) }
    };
    assert_eq!(
        answer_at(&mut guest, &reactor, "/limited", 1, 1).await,
        "allowed",
        "the window's one permit must admit the first request"
    );
    assert_eq!(
        answer_at(&mut guest, &reactor, "/limited", 200, 2).await,
        "rejected",
        "a second request in the same window must be rejected"
    );
}

#[tokio::test(start_paused = true)]
async fn timers_initialization_leaves_running_count_from_activation() {
    let files = package("alpha");
    let manifest = String::from_utf8(package_file(&files, "application.json"))
        .expect("the manifest is UTF-8")
        .replace("\"InitializeAsync\"", "\"LeaveTimersRunningAsync\"");
    assert!(
        manifest.contains("\"LeaveTimersRunningAsync\""),
        "the manifest must name its initializer for this test to replace: {manifest}"
    );
    let (product, request) = prepare_on_the_shared_runtime(with_package_file(
        files,
        "application.json",
        manifest.as_bytes(),
    ))
    .await;

    // Hours after the template was captured, on the wall clock the guest reads.
    let activated_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_millis() as u64
        + 5 * 60 * 60 * 1000;
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(activated_ms, 1);
    let mut guest = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { ReturnedGuest::prepared(decode(&product, &request), &reactor) }
    };
    assert_eq!(
        answer_at(&mut guest, &reactor, "/beats", activated_ms, 1).await,
        "beats=0",
        "step 1: a 30 s timer set during initialization must not be due on activation"
    );

    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert_eq!(
        answer_at(&mut guest, &reactor, "/beats", activated_ms + 30_100, 2).await,
        "beats=1",
        "step 2: the timer must fire once 30 s after activation"
    );
}

/// `alpha` prepared with `initializer` in place of its own.
async fn alpha_initialized_by(initializer: &str) -> (BuildProduct, BuildRequest) {
    let files = package("alpha");
    let manifest = String::from_utf8(package_file(&files, "application.json"))
        .expect("the manifest is UTF-8")
        .replace("\"InitializeAsync\"", &format!("\"{initializer}\""));
    assert!(
        manifest.contains(initializer),
        "the manifest must name its initializer for this test to replace: {manifest}"
    );
    prepare_on_the_shared_runtime(with_package_file(
        files,
        "application.json",
        manifest.as_bytes(),
    ))
    .await
}

/// A cell serving `product` as module `module`, whose instances stop after
/// `background_idle_limit_ms` without a request, and which shows guests'
/// the configured idle stop.
fn serving_cell(
    product: &BuildProduct,
    request: &BuildRequest,
    module: &isoplane_host::ModuleId,
    background_idle_limit_ms: u64,
) -> Arc<isoplane_host::cell::Cell> {
    let engine = Arc::new(
        IsoplaneEngine::new(engine_configuration(PlatformFiles::default()))
            .expect("the serving engine must initialize"),
    );
    let template = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { engine.load_prepared_product(&decode(product, request)) }
    }
    .expect("the prepared template must load");
    let services = Arc::new(isoplane_host::cell::Services {
        fetcher: Arc::new(isoplane_host::fakes::FakeFetcher::default()),
        hibernation: Arc::new(isoplane_host::fakes::FakeHibernationStore::default()),
        egress: Arc::new(isoplane_host::egress::HostEgressPolicy::new(
            Box::new(fixed_dns::FixedDns(vec![])),
            isoplane_host::egress::ConnectionLimits::uniform(8).expect("connection limits"),
        )),
    });
    let cell = isoplane_host::cell::Cell::new(
        isoplane_host::CellConfig {
            background_idle_limit_ms,
            ..isoplane_host::CellConfig::for_test("timer-wakes")
        },
        services,
        Arc::new(isoplane_host::fakes::FakeHibernator),
    )
    .expect("the cell must start");
    cell.register_module(
        "tenant",
        Arc::new(isoplane_host::activator::EngineActivator::new(
            engine,
            template,
            module.clone(),
        )),
        isoplane_abi::config::AppManifest {
            guest_memory_bytes: None,
            resource_limits: Default::default(),
            validation_limits: Default::default(),
            name: module.app.clone(),
            bindings: Default::default(),
            limits: isoplane_harness::GUEST_LIMITS,
            init_visible_vars: vec![],
        },
    )
    .expect("the module must register");
    cell
}

/// Asks `module` for `/beats`, answered by whichever instance the cell picks.
async fn ask_for_beats(
    cell: &Arc<isoplane_host::cell::Cell>,
    module: &isoplane_host::ModuleId,
) -> isoplane_host::DispatchResult {
    let request = isoplane_abi::dispatch::HttpRequest {
        method: "GET".into(),
        url: "https://alpha.test/beats".into(),
        path: "/beats".into(),
        query: vec![],
        headers: vec![],
        body: vec![],
    };
    let result = cell
        .dispatch(
            module,
            isoplane_host::cell::Target::Handler {
                route: "/beats".into(),
            },
            cell.http_record(module, request),
            std::time::Duration::from_secs(60),
        )
        .await
        .expect("the request must be answered");
    assert_eq!(result.status, 200, "the request must succeed: {result:?}");
    result
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while !done() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

fn alpha_request(
    cell: &isoplane_host::cell::Cell,
    module: &isoplane_host::ModuleId,
    path: &str,
) -> isoplane_abi::dispatch::DispatchRecord {
    cell.http_record(
        module,
        isoplane_abi::dispatch::HttpRequest {
            method: "GET".into(),
            url: format!("https://alpha.test{path}"),
            path: path.into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_that_unblocks_another_on_its_instance_leaves_it_to_resume_in_its_own_slice() {
    isoplane_harness::LogCapture::global();
    let prepared = prepared_package("alpha").await;
    let module = isoplane_host::ModuleId::new("alpha", "v1");
    let cell = serving_cell(&prepared.0, &prepared.1, &module, 300_000);
    let gate = |path: &str| isoplane_host::cell::Target::Handler { route: path.into() };

    let waiting = cell
        .submit(
            &module,
            gate("/gate/wait"),
            alpha_request(&cell, &module, "/gate/wait"),
        )
        .expect("step 1: the cell must admit the waiting request");
    wait_until(
        "step 1: the waiting request must park on its instance",
        || {
            cell.resident_ids()
                .first()
                .and_then(|id| cell.slot(*id))
                .is_some_and(|slot| {
                    slot.state() == isoplane_host::LifecycleTier::Parked
                        && cell.metrics().served_activated == 1
                })
        },
    )
    .await;

    let opened = cell
        .dispatch(
            &module,
            gate("/gate/open"),
            alpha_request(&cell, &module, "/gate/open"),
            std::time::Duration::from_secs(60),
        )
        .await
        .expect("step 2: the opening request must be answered");
    assert_eq!(
        (
            opened.status,
            opened.body.as_slice(),
            cell.metrics().served_shared
        ),
        (200, b"opened".as_slice(), 1),
        "step 2: the opening request must join the waiting request's instance: {opened:?}"
    );

    let resumed = tokio::task::spawn_blocking(move || {
        waiting.recv_timeout(std::time::Duration::from_secs(60))
    })
    .await
    .expect("the receiving task")
    .expect("step 3: the waiting request must finish once the gate opens");
    assert_eq!(
        (resumed.status, resumed.body.as_slice(), resumed.instance),
        (200, b"resumed".as_slice(), opened.instance),
        "step 3: the waiting request must resume on the same instance: {resumed:?}"
    );
    let resumed_in: Vec<u64> = cell
        .slot(opened.instance)
        .expect("step 4: the instance stays resident")
        .ctx
        .emitted_logs()
        .into_iter()
        .filter(|log| log.fields.contains("resumed after the gate opened"))
        .map(|log| log.dispatch)
        .collect();
    assert_eq!(
        resumed_in,
        vec![resumed.dispatch_id],
        "step 4: the waiting request's continuation must run in its own dispatch's slice, not the one that opened the gate"
    );
    cell.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_timers_run_by_default_without_recording_http_requests_and_stop_after_inactivity() {
    isoplane_harness::LogCapture::global();
    let (product, request) = alpha_initialized_by("StartHeartbeatAsync").await;
    let module = isoplane_host::ModuleId::new("alpha", "v1");
    let cell = serving_cell(&product, &request, &module, 1_000);
    let meters = cell.meters(&module).expect("the module has meters");

    let first = ask_for_beats(&cell, &module).await;
    wait_until(
        "step 1: the idle instance must be woken for its timers without a request",
        || {
            cell.slot(first.instance).is_some_and(|slot| {
                slot.ctx.emitted_logs().iter().any(|log| {
                    log.fields.contains("heartbeat")
                        && log.dispatch == isoplane_host::futures::INSTANCE_WORK
                })
            })
        },
    )
    .await;
    let instance = cell
        .slot(first.instance)
        .expect("step 2: the woken instance must still be resident");
    assert!(instance
        .ctx
        .emitted_logs()
        .iter()
        .any(|log| log.fields.contains("heartbeat")
            && log.dispatch == isoplane_host::futures::INSTANCE_WORK));
    assert_eq!(
        meters.dispatches(),
        1,
        "background wakes are not HTTP requests"
    );

    wait_until(
        "step 3: the instance must stop once it has gone 1 s without a request",
        // Not merely out of the pool, which it also is while a wake runs.
        || cell.slot(first.instance).is_none(),
    )
    .await;
    let after = ask_for_beats(&cell, &module).await;
    assert_ne!(
        after.instance, first.instance,
        "step 5: a request after the stop must start a new instance"
    );
    cell.shutdown();
}

async fn native_use(path: &str) -> String {
    answer_package("native", path).await
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

/// The `sqlite` package's answer for one use of SQLite, after a park and
/// across a Store restoration.
async fn sqlite_use(path: &str) -> String {
    answer_package("sqlite", path).await
}

#[tokio::test(start_paused = true)]
async fn an_in_memory_sqlite_database_runs_the_applications_function_and_collation() {
    assert_eq!(
        sqlite_use("/memory").await,
        "cba;6",
        "Microsoft.Data.Sqlite must read back what it wrote, ordering by the package's collation and calling its function"
    );
}

#[tokio::test(start_paused = true)]
async fn a_named_shared_cache_sqlite_database_is_shared_between_connections() {
    assert_eq!(
        sqlite_use("/shared").await,
        "Auckland,Paris,Tokyo",
        "a connection to a named in-memory database must see what another connection wrote to it"
    );
}

#[tokio::test(start_paused = true)]
async fn an_in_memory_sqlite_database_loaded_as_the_instance_starts_serves_its_requests() {
    assert_eq!(
        sqlite_use("/reference").await,
        "France,New Zealand",
        "a shared-cache database the package's activation filled must be there for a request's own connection"
    );
}

#[tokio::test(start_paused = true)]
async fn ef_core_over_sqlite_runs_its_registered_functions_collation_and_aggregate() {
    assert_eq!(
        sqlite_use("/catalog").await,
        "saucer,cup,kettle;43.75",
        "EF Core's decimal comparison, ordering and sum must run through the functions, collation and aggregate it registers with SQLite"
    );
}

#[tokio::test(start_paused = true)]
async fn ef_core_over_sqlite_runs_with_the_sqlitepclraw_release_the_application_chooses() {
    assert_eq!(
        answer_package("sqlite-pclraw3", "/catalog").await,
        "saucer,cup,kettle;43.75",
        "the runtime's e_sqlite3 must serve SQLitePCLRaw 3's imports and callbacks as it serves the 2.x release EF Core selects"
    );
}

#[tokio::test(start_paused = true)]
async fn a_file_backed_sqlite_database_is_refused_naming_in_memory_instead() {
    let answer = sqlite_use("/file").await;
    assert!(
        answer.starts_with("SqliteException: SQLite Error 14: 'file-backed databases are not available")
            && answer.contains("Data Source=:memory:"),
        "opening a database file must fail saying why and naming the in-memory alternative: {answer}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_locked_sqlite_table_fails_the_command_instead_of_waiting() {
    // Microsoft.Data.Sqlite retries a locked statement after Thread.Sleep,
    // which throws in a guest rather than wait while the lock's holder cannot run.
    assert_eq!(
        sqlite_use("/locked").await,
        "PlatformNotSupportedException: Arg_PlatformNotSupported",
        "Microsoft.Data.Sqlite's retry on a locked table must fail the command, not wait inside the call"
    );
}

#[tokio::test(start_paused = true)]
async fn sqlites_generator_is_reseeded_in_each_activation() {
    // The package's initializer draws from SQLite's generator, so the
    // template holds its seeded state.
    let prepared = prepared_package("sqlite").await;
    let mut draws = Vec::new();
    for instance in [7301, 7302].map(isoplane_host::InstanceId) {
        let answer = answer_on_an_instance(&prepared.0, &prepared.1, instance, "/random").await;
        let draw: i64 = answer.parse().unwrap_or_else(|error| {
            panic!("SELECT random() must answer an integer, not {answer:?}: {error}")
        });
        draws.push(draw);
    }
    assert_ne!(
        draws[0], draws[1],
        "two activations of one template must not replay the SQLite generator state the snapshot holds"
    );
}

/// The first dispatch of `path` on the prepared `diagnostics` package, and
/// what the guest logged.
async fn diagnose(path: &str) -> (isoplane_engine::DispatchOutcome, Vec<String>) {
    isoplane_harness::LogCapture::global();
    let prepared = prepared_package("diagnostics").await;
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let mut guest = {
        // SAFETY: The private decode helper receives only products from this test module's local preparation paths.
        unsafe { ReturnedGuest::prepared(decode(&prepared.0, &prepared.1), &reactor) }
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
