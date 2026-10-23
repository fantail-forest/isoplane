//! A harness for tests that drive a managed guest through the real reactor:
//! a cell runtime with in-memory services and an injected egress policy, and
//! a guest whose every host return destroys its Store and whose every wake
//! restores it on another thread, exercising returned-state reconstruction.
//!
//! Test support only: it panics on every failure because its callers are
//! tests. A dev-dependency only; nothing here is linked into a daemon.

mod compiled_guest;

pub use compiled_guest::engine_configuration;
use compiled_guest::engine_serving;
use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::config::Limits;
use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use isoplane_abi::result::ResultRecord;
use isoplane_engine::prepared::PreparedTemplate;
use isoplane_engine::{
    Completion, DispatchOutcome, Instance, InstanceImage, InstanceOps, IsoplaneEngine,
    OccupancyBudget, PlatformFiles, Template,
};

use isoplane_host::bindings::{
    CellBindings, CellRuntime, InstanceCtx, Services, Tenancy, WakeSink,
};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::fetcher::PinnedHttpFetcher;
use isoplane_host::futures::FutureTable;
use isoplane_host::meters::Meters;
use isoplane_host::metrics::Metrics;
use isoplane_host::response::Response;
use isoplane_host::{InstanceId, ModuleId};

/// How long a test waits on the reactor, a peer or a resolver before it is a hung test.
pub const WAKE_DEADLINE: Duration = Duration::from_secs(60);

/// The production plan used for preparation and execution of returned guests.
pub const GUEST_LIMITS: Limits = Limits {
    instances_per_application: 8,
    module_bytes_compressed: 52428800,
    package_bytes: 52428800,
    instance_memory_max: 134217728,
    occupancy_ms_per_dispatch: 30000,
    instance_open_connections: 32,
    pending_futures_per_dispatch: 64,
};

/// Controlled network dependencies with production budgets, which tests cannot override.
///
/// ```compile_fail
/// let limits = isoplane_host::egress::ConnectionLimits::uniform(1).unwrap();
/// let policy = isoplane_host::egress::HostEgressPolicy::system(limits).unwrap();
/// let network = isoplane_harness::Network(policy);
/// ```
pub struct Network(HostEgressPolicy);

/// The node's default outbound connection limits, which [`Network`] enforces.
pub fn production_connection_limits() -> isoplane_host::egress::ConnectionLimits {
    let limits = isoplane_runtime::network::NetworkLimits::default();
    isoplane_host::egress::ConnectionLimits::new(isoplane_host::egress::ConnectionCeilings {
        in_progress: limits.instance_pending_connections,
        open: limits.instance_open_connections,
    })
    .expect("the production connection limits must be valid")
}

impl Default for Network {
    fn default() -> Self {
        Self(
            HostEgressPolicy::system(production_connection_limits())
                .expect("the production egress policy must initialize"),
        )
    }
}

impl Network {
    /// Uses a controlled resolver while retaining the production network budgets.
    pub fn with_resolver(resolver: Box<dyn isoplane_host::Resolver>) -> Self {
        Self(HostEgressPolicy::new(
            resolver,
            production_connection_limits(),
        ))
    }

    /// Restricts destinations to test peers or exercises a destination-policy refusal.
    pub fn with_destinations(
        mut self,
        destinations: Arc<dyn isoplane_host::egress::DestinationPolicy>,
    ) -> Self {
        self.0 = self.0.with_destinations(destinations);
        self
    }

    /// Controls connection completion or transport buffering for networking tests.
    pub fn with_connections(
        mut self,
        connections: Arc<dyn isoplane_host::egress::ConnectionFactory>,
    ) -> Self {
        self.0 = self.0.with_connections(connections);
        self
    }
}

struct Wake(tokio::sync::Notify);

impl WakeSink for Wake {
    fn signal(&self, _: InstanceId) {
        self.0.notify_one();
    }
}

/// One instance's host resources with production limits and a real reactor.
pub struct Reactor {
    /// The instance's host context: resources and responses.
    pub context: Arc<InstanceCtx>,
    /// The future table the reactor completes host work into.
    pub futures: Arc<FutureTable>,
    wake: Arc<Wake>,
    platform_files: PlatformFiles,
}

impl Reactor {
    /// A reactor using the production host's default outbound policy and limits.
    pub fn new(owner: InstanceId) -> Self {
        Self::with_network(owner, Network::default())
    }

    /// A reactor on the current tokio runtime for `owner`, whose outbound
    /// connections, lookups and fetches go through `network` and the real
    /// transport. The guest's PAL filesystem holds no files, including no
    /// default trust bundle; TLS tests supply their trust explicitly.
    pub fn with_network(owner: InstanceId, network: Network) -> Self {
        Self::with_platform_files(owner, network, PlatformFiles::default())
    }

    /// As [`Self::with_network`], with `files` served read-only through the guest's
    /// PAL filesystem.
    pub fn with_platform_files(owner: InstanceId, network: Network, files: PlatformFiles) -> Self {
        let egress = Arc::new(network.0);
        let fetcher = PinnedHttpFetcher::new(
            isoplane_runtime::network::NetworkLimits::default().inline_response_bytes,
        );
        let futures = Arc::new(FutureTable::new(
            isoplane_host::CellConfig::default().buffered_future_bytes,
        ));
        let wake = Arc::new(Wake(tokio::sync::Notify::new()));
        let runtime = Arc::new(CellRuntime {
            runtime_configuration: Default::default(),
            services: Arc::new(Services {
                egress,
                fetcher: Arc::new(fetcher),
            }),
            futures: futures.clone(),
            metrics: Arc::new(Metrics::new()),
            wake: wake.clone(),
            reactor: tokio::runtime::Handle::current(),
        });
        Self {
            context: context(owner, runtime),
            futures,
            wake,
            platform_files: files,
        }
    }

    /// Another instance of the same application on this reactor's runtime,
    /// sharing its outbound policy, limits and future table, as instances of
    /// one cell do.
    pub fn sibling(&self, owner: InstanceId) -> Self {
        let runtime = self
            .context
            .rt
            .clone()
            .expect("the test reactor always carries a runtime");
        Self {
            context: context(owner, runtime),
            futures: self.futures.clone(),
            wake: self.wake.clone(),
            platform_files: self.platform_files.clone(),
        }
    }

    /// The runtime this reactor's context carries.
    pub fn runtime(&self) -> &CellRuntime {
        self.context
            .rt
            .as_ref()
            .expect("the test reactor always carries a runtime")
    }

    /// The body of the `index`th response the guest created (1-based).
    pub fn response_body(&self, index: u32) -> Vec<u8> {
        self.context
            .responses
            .lock()
            .get(index)
            .unwrap_or_else(|| panic!("the guest must have created response {index}"))
            .body()
    }

    /// The body of the `index`th response as text.
    pub fn response_text(&self, index: u32) -> String {
        String::from_utf8_lossy(&self.response_body(index)).into_owned()
    }
}

fn context(owner: InstanceId, runtime: Arc<CellRuntime>) -> Arc<InstanceCtx> {
    LogCapture::global();
    Arc::new(InstanceCtx::new(
        owner,
        Tenancy {
            module: ModuleId::new("returned-guest", "v1"),
            tenant: "returned-guest-test".into(),
            limits: GUEST_LIMITS,

            allowlist: None,
        },
        Arc::new(Meters::new()),
        Some(runtime),
    ))
}

/// What a guest's template is loaded from on each engine that serves it.
#[derive(Clone)]
enum Source {
    Module {
        bytes: Arc<[u8]>,
        application_files: PlatformFiles,
    },
    /// The shared runtime as Task compiled it, serving an application's files.
    SharedRuntime {
        application_files: PlatformFiles,
    },
    Prepared(Arc<PreparedTemplate>),
    #[cfg(target_os = "linux")]
    Layered {
        base: Arc<PreparedTemplate>,
        layer: Arc<PreparedTemplate>,
    },
}

impl Source {
    fn serve(&self, files: PlatformFiles) -> (IsoplaneEngine, Template) {
        match self {
            Source::Module {
                bytes,
                application_files,
            } => {
                let (engine, template) = engine_serving(bytes, files);
                let template = template
                    .with_application_files(application_files.clone())
                    .expect("bind application files to the returned guest");
                (engine, template)
            }
            Source::SharedRuntime { application_files } => {
                let engine = IsoplaneEngine::new(engine_configuration(files))
                    .expect("the returned-guest engine must initialize");
                let template = isoplane_fixtures::load_compiled_runtime(&engine)
                    .with_application_files(application_files.clone())
                    .expect("bind application files to the returned guest");
                (engine, template)
            }
            Source::Prepared(prepared) => {
                let engine = IsoplaneEngine::new(engine_configuration(files))
                    .expect("the returned-guest engine must initialize");
                let template = engine
                    .load_prepared_product(prepared)
                    .unwrap_or_else(|error| panic!("the prepared template must load: {error:#}"));
                (engine, template)
            }
            #[cfg(target_os = "linux")]
            Source::Layered { base, layer } => {
                let engine = IsoplaneEngine::new(engine_configuration(files))
                    .expect("the returned-guest engine must initialize");
                let base = engine
                    .load_prepared_product(base)
                    .unwrap_or_else(|error| panic!("the base template must load: {error:#}"));
                let template = engine
                    .load_prepared_product_over(&base, layer)
                    .unwrap_or_else(|error| panic!("the layer must load over its base: {error:#}"));
                (engine, template)
            }
        }
    }
}

/// A guest that returns to the host on an empty stack: parked or done, its
/// Store is destroyed, and the next call restores it on another thread.
pub struct ReturnedGuest {
    engine: IsoplaneEngine,
    template: Template,
    source: Source,
    platform_files: PlatformFiles,
    context: Arc<InstanceCtx>,
    snapshot: Option<(InstanceImage, Vec<u8>)>,
}

impl ReturnedGuest {
    /// Loads `module` for `reactor`'s instance.
    pub fn new(module: &[u8], reactor: &Reactor) -> Self {
        Self::with_application_files(module, Default::default(), reactor)
    }

    /// Loads a runtime or prepared module with its immutable application package.
    pub fn with_application_files(
        module: &[u8],
        application_files: PlatformFiles,
        reactor: &Reactor,
    ) -> Self {
        Self::serving(
            Source::Module {
                bytes: module.into(),
                application_files,
            },
            reactor,
        )
    }

    /// Loads a managed behavior probe on the shared runtime for `reactor`.
    pub fn managed_probe(
        name: &str,
        runtime: isoplane_fixtures::GuestRuntime,
        reactor: &Reactor,
    ) -> Self {
        let isoplane_fixtures::GuestRuntime::Mono = runtime;
        let application_files = isoplane_fixtures::managed_probe(name)
            .into_iter()
            .map(|file| isoplane_engine::PlatformFile {
                path: file.path,
                bytes: file.bytes.into(),
                modified_ns: 0,
            })
            .collect();
        Self::serving(
            Source::SharedRuntime {
                application_files: PlatformFiles::new(application_files)
                    .expect("valid managed probe package"),
            },
            reactor,
        )
    }

    /// Serves trusted preparation output for `reactor`'s instance, as a cell
    /// loads what its builder produced. The preparation must have used
    /// [`engine_configuration`] with the reactor's platform files.
    pub fn prepared(prepared: PreparedTemplate, reactor: &Reactor) -> Self {
        Self::serving(Source::Prepared(Arc::new(prepared)), reactor)
    }

    /// Serves trusted preparation output captured as a layer over `base`, as
    /// [`ReturnedGuest::prepared`] serves a whole template.
    #[cfg(target_os = "linux")]
    pub fn layered(base: PreparedTemplate, layer: PreparedTemplate, reactor: &Reactor) -> Self {
        Self::serving(
            Source::Layered {
                base: Arc::new(base),
                layer: Arc::new(layer),
            },
            reactor,
        )
    }

    fn serving(source: Source, reactor: &Reactor) -> Self {
        let (engine, template) = source.serve(reactor.platform_files.clone());
        Self {
            engine,
            template,
            source,
            platform_files: reactor.platform_files.clone(),
            context: reactor.context.clone(),
            snapshot: None,
        }
    }

    /// Serves `files` to the guest from its next activation on: the engine
    /// is rebuilt around the new set (a node's set is fixed for its life, so
    /// this is a new node process serving the guest's saved state) and the
    /// guest's saved memory carries over unchanged.
    pub fn serve_platform_files(&mut self, files: PlatformFiles) {
        let (engine, template) = self.source.serve(files.clone());
        self.engine = engine;
        self.template = template;
        self.platform_files = files;
    }

    /// A second guest on the same engine whose first activation restores
    /// this guest's saved memory: what activating a template that already
    /// holds the guest's cached state amounts to. This guest keeps its own
    /// copy.
    pub fn activated_from_image(&self, reactor: &Reactor) -> Self {
        let (engine, template) = self.source.serve(self.platform_files.clone());
        Self {
            engine,
            template,
            source: self.source.clone(),
            platform_files: self.platform_files.clone(),
            context: reactor.context.clone(),
            snapshot: self.snapshot.clone(),
        }
    }

    /// Dispatches `GET path` on a fresh Store restored from the last return.
    pub fn start(&mut self, path: &str) -> DispatchOutcome {
        self.start_request(request("GET", path, vec![]))
    }

    /// Dispatches `request` on a fresh Store restored from the last return.
    pub fn start_request(&mut self, request: HttpRequest) -> DispatchOutcome {
        let mut instance = self.activate();
        let outcome = instance.dispatch(
            &DispatchRecord {
                dispatch_id: self.context.dispatch_id(),
                kind: DispatchKind::Http(request),
                trace: TraceContext::default(),
                cancel_future: 0,
                occupancy_budget_ms: self.context.limits.occupancy_ms_per_dispatch,
                now_ms: self.context.slice_clock_ms(),
            },
            budget(&self.context),
        );
        self.snapshot = capture(&mut instance, &outcome);
        outcome
    }

    async fn await_completion(&self, reactor: &Reactor) -> (u64, ResultRecord) {
        tokio::time::timeout(
            WAKE_DEADLINE,
            next_completion(&reactor.futures, self.context.instance, &reactor.wake.0),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the reactor must deliver a completion before the test deadline; logs: {:?}",
                self.context.emitted_logs()
            )
        })
    }

    /// Waits for the reactor to signal readiness, then delivers the ready
    /// completion to the guest restored on another thread.
    pub async fn wake(&mut self, reactor: &Reactor) -> DispatchOutcome {
        let (future, result) = self.await_completion(reactor).await;
        self.restore_and_wake(future, &result)
    }

    /// Polls the saved guest without delivering a host completion.
    pub fn poll(&mut self) -> DispatchOutcome {
        self.restore_and_wake(0, &ResultRecord::Ok(Vec::new()))
    }
    /// Starts `path` and wakes the guest until it finishes, failing after
    /// `max_wakes` wakes so a guest that never finishes is a failed test
    /// rather than a hung one.
    pub async fn run(
        &mut self,
        reactor: &Reactor,
        path: &str,
        max_wakes: usize,
    ) -> DispatchOutcome {
        let mut outcome = self.start(path);
        let mut wakes = 0;
        while outcome.completion == Completion::Parked {
            assert!(
                wakes < max_wakes,
                "{path} must finish within {max_wakes} wakes; logs: {:?}",
                self.context.emitted_logs()
            );
            outcome = self.wake(reactor).await;
            wakes += 1;
        }
        outcome
    }

    /// Runs `path` to completion and returns the text it answered with,
    /// failing on any outcome but a completed dispatch.
    pub async fn answer(&mut self, reactor: &Reactor, path: &str, max_wakes: usize) -> String {
        let outcome = self.run(reactor, path, max_wakes).await;
        self.text_of(path, outcome)
    }

    /// Wakes a started, parked dispatch until it finishes and returns the
    /// text it answered with, failing after `max_wakes` wakes.
    pub async fn finish(&mut self, reactor: &Reactor, max_wakes: usize) -> String {
        self.finish_counting(reactor, max_wakes).await.0
    }

    /// As [`Self::finish`], also returning how many wakes it took.
    pub async fn finish_counting(
        &mut self,
        reactor: &Reactor,
        max_wakes: usize,
    ) -> (String, usize) {
        let mut wakes = 0;
        let mut outcome = self.wake(reactor).await;
        wakes += 1;
        while outcome.completion == Completion::Parked {
            assert!(
                wakes < max_wakes,
                "the dispatch must finish within {max_wakes} wakes; logs: {:?}",
                self.context.emitted_logs()
            );
            outcome = self.wake(reactor).await;
            wakes += 1;
        }
        (self.text_of("the dispatch", outcome), wakes)
    }

    /// The text a completed dispatch answered with, failing on any other
    /// outcome; for a test that drives the wakes itself.
    pub fn text_of_outcome(&self, outcome: DispatchOutcome) -> String {
        self.text_of("the dispatch", outcome)
    }

    /// The response a completed dispatch answered with, status and headers
    /// included, failing on any other outcome; for a test that drives the
    /// wakes itself.
    pub fn response_of_outcome(&self, outcome: DispatchOutcome) -> Response {
        self.response_of("the dispatch", outcome)
    }

    fn text_of(&self, what: &str, outcome: DispatchOutcome) -> String {
        String::from_utf8_lossy(&self.response_of(what, outcome).body()).into_owned()
    }

    fn response_of(&self, what: &str, outcome: DispatchOutcome) -> Response {
        let isoplane_abi::status::PollStatus::Done { response } = outcome.status else {
            panic!(
                "{what} must complete: {outcome:?}; logs: {:?}",
                self.context.emitted_logs()
            );
        };
        self.context
            .responses
            .lock()
            .take(response)
            .unwrap_or_else(|| panic!("{what} must produce a response"))
    }

    fn activate(&mut self) -> Instance {
        let mut instance = self
            .engine
            .activate(&self.template, CellBindings::boxed(self.context.clone()))
            .expect("the guest must activate");
        if let Some((image, memory)) = self.snapshot.take() {
            instance
                .restore_mutable_state(&image)
                .expect("the returning guest must restore its saved state");
            instance.memory().copy_from_slice(&memory);
        }
        instance
    }

    fn restore_and_wake(&mut self, future: u64, result: &ResultRecord) -> DispatchOutcome {
        let (image, memory) = self
            .snapshot
            .take()
            .expect("a wake must follow an empty-stack park");
        let (outcome, snapshot) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let mut instance = self
                        .engine
                        .activate(&self.template, CellBindings::boxed(self.context.clone()))
                        .expect("the guest must activate on another worker");
                    instance
                        .restore_mutable_state(&image)
                        .expect("the guest globals must restore");
                    instance.memory().copy_from_slice(&memory);
                    let outcome = instance.wake(
                        self.context.dispatch_id(),
                        future,
                        result,
                        budget(&self.context),
                    );
                    let snapshot = capture(&mut instance, &outcome);
                    (outcome, snapshot)
                })
                .join()
                .expect("the worker must not panic")
        });
        self.snapshot = snapshot;
        outcome
    }
}

/// A bodiless `method path` request to the guest's host, with `headers`.
pub fn request(method: &str, path: &str, headers: Vec<(String, String)>) -> HttpRequest {
    HttpRequest {
        method: method.into(),
        url: format!("https://guest.test{path}"),
        path: path.into(),
        query: vec![],
        headers,
        body: vec![],
    }
}

fn capture(instance: &mut Instance, outcome: &DispatchOutcome) -> Option<(InstanceImage, Vec<u8>)> {
    matches!(outcome.completion, Completion::Parked | Completion::Done).then(|| {
        (
            instance
                .capture_mutable_state()
                .expect("an empty-stack return must be capturable"),
            instance.memory().to_vec(),
        )
    })
}

fn budget(context: &InstanceCtx) -> OccupancyBudget {
    OccupancyBudget {
        occupancy_ms: context.limits.occupancy_ms_per_dispatch,
        slice_wall_ms_cap: isoplane_host::CellConfig::default().slice_wall_ms_cap,
        spent_ns: 0,
    }
}

async fn next_completion(
    futures: &FutureTable,
    owner: InstanceId,
    wake: &tokio::sync::Notify,
) -> (u64, ResultRecord) {
    loop {
        if let Some(completion) = futures
            .take_ready(owner)
            .map(isoplane_host::futures::Ready::into_pair)
        {
            return completion;
        }
        // Notifications coalesce and can outlive a cancelled completion.
        // The table, not a wake permit, determines whether work is ready.
        wake.notified().await;
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use isoplane_host::futures::WakeTarget;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn complete(table: &FutureTable, owner: InstanceId, id: u64) {
        assert_eq!(
            table.complete(id, ResultRecord::Ok(vec![7])),
            WakeTarget::Instance(owner),
            "the completion must belong to the waiting guest"
        );
    }

    fn alloc(table: &FutureTable, owner: InstanceId) -> u64 {
        table
            .alloc(owner, 1, "sleep", GUEST_LIMITS.pending_futures_per_dispatch)
            .expect("the test dispatch must have a free future slot")
    }

    #[test]
    fn coalesced_notifications_do_not_hide_a_second_ready_completion() {
        let table = FutureTable::new(isoplane_host::CellConfig::default().buffered_future_bytes);
        let owner = InstanceId(1);
        let wake = tokio::sync::Notify::new();
        let first = alloc(&table, owner);
        let second = alloc(&table, owner);
        complete(&table, owner, first);
        complete(&table, owner, second);
        wake.notify_one();
        let mut context = Context::from_waker(Waker::noop());
        for id in [first, second] {
            let mut waiter = std::pin::pin!(next_completion(&table, owner, &wake));
            assert_eq!(
                waiter.as_mut().poll(&mut context),
                Poll::Ready((id, ResultRecord::Ok(vec![7]))),
                "each ready completion must be delivered even when wake permits coalesce"
            );
        }
    }

    #[test]
    fn a_cancelled_completion_notification_does_not_disrupt_the_next_completion() {
        let table = FutureTable::new(isoplane_host::CellConfig::default().buffered_future_bytes);
        let owner = InstanceId(1);
        let wake = tokio::sync::Notify::new();
        let cancelled = alloc(&table, owner);
        complete(&table, owner, cancelled);
        wake.notify_one();
        assert!(
            table.cancel_owned(owner, cancelled.into()),
            "the old completion must be cancelled"
        );
        let pending = alloc(&table, owner);
        let mut waiter = std::pin::pin!(next_completion(&table, owner, &wake));
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(
            waiter.as_mut().poll(&mut context),
            Poll::Pending,
            "a stale notification must wait for a real completion"
        );
        complete(&table, owner, pending);
        wake.notify_one();
        assert_eq!(
            waiter.as_mut().poll(&mut context),
            Poll::Ready((pending, ResultRecord::Ok(vec![7]))),
            "the replacement completion must still wake its guest"
        );
    }
}

/// Test writers that observe guest events at the formatted tracing boundary.
pub mod logging {
    use std::io::Write;
    use std::sync::{Arc, Mutex, OnceLock};

    /// Formatted bytes emitted by the guest logging subscriber under test.
    #[derive(Clone, Default)]
    pub struct LogCapture(Arc<Mutex<Vec<u8>>>);

    /// A guest event decoded from the host's JSON formatter.
    #[derive(Clone, Debug, serde::Deserialize)]
    pub struct GuestLog {
        /// Host-established instance identifier.
        pub instance: String,
        /// ABI severity recorded by the host.
        pub level: u8,
        /// Host-established dispatch identifier.
        pub dispatch: u64,
        /// Untrusted guest message text.
        pub fields: String,
        /// Remaining fields emitted by the host boundary.
        #[serde(flatten)]
        pub metadata: serde_json::Map<String, serde_json::Value>,
    }

    impl LogCapture {
        /// Installs one guest-event writer before a test starts execution threads.
        pub fn global() -> &'static Self {
            use tracing_subscriber::prelude::*;
            static CAPTURE: OnceLock<LogCapture> = OnceLock::new();
            CAPTURE.get_or_init(|| {
                let capture = Self::default();
                let subscriber = tracing_subscriber::registry().with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .with_ansi(false)
                        .with_writer(capture.clone())
                        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                            metadata.target().contains("host::bindings")
                        })),
                );
                tracing::subscriber::set_global_default(subscriber)
                    .expect("one guest-event subscriber per test process");
                capture
            })
        }

        /// Decodes emitted events for `instance` in their publication order.
        pub fn for_instance(&self, instance: impl std::fmt::Display) -> Vec<GuestLog> {
            let instance = instance.to_string();
            let bytes = self.0.lock().expect("test writer mutex").clone();
            String::from_utf8(bytes)
                .expect("tracing emits UTF-8")
                .lines()
                .filter_map(|line| {
                    let event: serde_json::Value =
                        serde_json::from_str(line).expect("tracing JSON event");
                    if event["fields"]["message"] != "guest log"
                        || event["fields"]["instance"] != instance
                    {
                        return None;
                    }
                    Some(
                        serde_json::from_value(event["fields"].clone())
                            .expect("the guest logging boundary emits complete records"),
                    )
                })
                .collect()
        }
    }

    impl Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("test writer mutex")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogCapture {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    /// Guest diagnostics collected outside the production instance's state.
    pub trait EmittedGuestLogs {
        /// Returns actual output attributed to this instance by the host.
        fn emitted_logs(&self) -> Vec<GuestLog>;
    }

    impl EmittedGuestLogs for isoplane_host::bindings::InstanceCtx {
        fn emitted_logs(&self) -> Vec<GuestLog> {
            LogCapture::global().for_instance(self.instance)
        }
    }
}

pub use logging::{EmittedGuestLogs, GuestLog, LogCapture};
