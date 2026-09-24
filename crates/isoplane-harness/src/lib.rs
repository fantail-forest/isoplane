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

use isoplane_abi::config::{HandlerLifecycle, Limits};
use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use isoplane_abi::result::ResultRecord;
use isoplane_engine::prepared::PreparedTemplate;
use isoplane_engine::{
    Completion, CpuBudget, DispatchOutcome, Instance, InstanceImage, InstanceOps, IsoplaneEngine,
    PlatformFiles, Template,
};

use isoplane_host::bindings::{
    CellBindings, CellRuntime, InstanceCtx, Services, Tenancy, WakeSink,
};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::fetcher::PinnedHttpFetcher;
use isoplane_host::futures::FutureTable;
use isoplane_host::meters::Meters;
use isoplane_host::metrics::Metrics;
use isoplane_host::{InstanceId, ModuleId};

/// How long a test waits on the reactor, a peer or a resolver before it is a hung test.
pub const WAKE_DEADLINE: Duration = Duration::from_secs(60);

struct Wake(tokio::sync::Notify);

impl WakeSink for Wake {
    fn signal(&self, _: InstanceId) {
        self.0.notify_one();
    }
}

/// One instance's host resources on a real reactor, with the outbound policy
/// the test supplies.
pub struct Reactor {
    /// The instance's host context: resources and responses.
    pub context: Arc<InstanceCtx>,
    /// The future table the reactor completes host work into.
    pub futures: Arc<FutureTable>,
    wake: Arc<Wake>,
    platform_files: PlatformFiles,
}

impl Reactor {
    /// A reactor on the current tokio runtime for `owner`, whose outbound
    /// connections, lookups and fetches go through `policy` and the real
    /// transport. The guest's PAL filesystem holds no files, including no
    /// default trust bundle; TLS tests supply their trust explicitly.
    pub fn new(owner: InstanceId, policy: HostEgressPolicy) -> Self {
        Self::with_platform_files(owner, policy, PlatformFiles::default())
    }

    /// As [`Self::new`], with `files` served read-only through the guest's
    /// PAL filesystem.
    pub fn with_platform_files(
        owner: InstanceId,
        policy: HostEgressPolicy,
        files: PlatformFiles,
    ) -> Self {
        LogCapture::global();
        let egress = Arc::new(policy);
        let fetcher = PinnedHttpFetcher::new(egress.clone(), 256 * 1024);
        let futures = Arc::new(FutureTable::new());
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
        let context = Arc::new(InstanceCtx::new(
            owner,
            Tenancy {
                module: ModuleId::new("returned-guest", "v1"),
                tenant: "returned-guest-test".into(),
                limits: Limits::default(),

                lifecycle: HandlerLifecycle::Reuse,
                allowlist: None,
            },
            Arc::new(Meters::new()),
            Some(runtime),
        ));
        Self {
            context,
            futures,
            wake,
            platform_files: files,
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

/// What a guest's template is loaded from on each engine that serves it.
#[derive(Clone)]
enum Source {
    Module(Arc<[u8]>),
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
            Source::Module(module) => engine_serving(module, files),
            Source::Prepared(prepared) => {
                let engine = IsoplaneEngine::new(engine_configuration(files))
                    .expect("the returned-guest engine must initialize");
                let template = {
                    // SAFETY: the private Source variants are constructed only by
                    // ReturnedGuest's unsafe prepared/layered constructors, whose
                    // callers establish controlled local compiler origin.
                    unsafe { engine.load_prepared_product(prepared) }
                }
                .unwrap_or_else(|error| panic!("the prepared template must load: {error:#}"));
                (engine, template)
            }
            #[cfg(target_os = "linux")]
            Source::Layered { base, layer } => {
                let engine = IsoplaneEngine::new(engine_configuration(files))
                    .expect("the returned-guest engine must initialize");
                let base = {
                    // SAFETY: the private Source variants are constructed only by
                    // ReturnedGuest's unsafe prepared/layered constructors, whose
                    // callers establish controlled local compiler origin.
                    unsafe { engine.load_prepared_product(base) }
                }
                .unwrap_or_else(|error| panic!("the base template must load: {error:#}"));
                let template = {
                    // SAFETY: the private Source variants are constructed only by
                    // ReturnedGuest's unsafe prepared/layered constructors, whose
                    // callers establish controlled local compiler origin.
                    unsafe { engine.load_prepared_product_over(&base, layer) }
                }
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
        Self::serving(Source::Module(module.into()), reactor)
    }

    /// Serves trusted preparation output for `reactor`'s instance, as a cell
    /// loads what its builder produced. The preparation must have used
    /// [`engine_configuration`] with the reactor's platform files.
    ///
    /// # Safety
    /// Every native payload must be unchanged output from controlled local preparation.
    /// Customer-supplied or rewritten serialized native code is not admissible.
    pub unsafe fn prepared(prepared: PreparedTemplate, reactor: &Reactor) -> Self {
        Self::serving(Source::Prepared(Arc::new(prepared)), reactor)
    }

    /// Serves trusted preparation output captured as a layer over `base`, as
    /// [`ReturnedGuest::prepared`] serves a whole template.
    #[cfg(target_os = "linux")]
    ///
    /// # Safety
    /// Every native payload must be unchanged output from controlled local preparation.
    /// Customer-supplied or rewritten serialized native code is not admissible.
    pub unsafe fn layered(
        base: PreparedTemplate,
        layer: PreparedTemplate,
        reactor: &Reactor,
    ) -> Self {
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
        let mut instance = self.activate();
        let outcome = instance.dispatch(
            &DispatchRecord {
                dispatch_id: self.context.dispatch_id(),
                kind: DispatchKind::Http(HttpRequest {
                    method: "GET".into(),
                    url: format!("https://guest.test{path}"),
                    path: path.into(),
                    query: vec![],
                    headers: vec![],
                    body: vec![],
                }),
                trace: TraceContext::default(),
                cancel_future: 0,
                cpu_budget_ms: 30_000,
                now_ms: self.context.slice_clock_ms(),
            },
            budget(),
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

    fn text_of(&self, what: &str, outcome: DispatchOutcome) -> String {
        let isoplane_abi::status::PollStatus::Done { response } = outcome.status else {
            panic!(
                "{what} must complete: {outcome:?}; logs: {:?}",
                self.context.emitted_logs()
            );
        };
        String::from_utf8_lossy(
            &self
                .context
                .responses
                .lock()
                .take(response)
                .unwrap_or_else(|| panic!("{what} must produce a response"))
                .body(),
        )
        .into_owned()
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
                    let outcome = instance.wake(future, result, budget());
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

fn budget() -> CpuBudget {
    CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 60_000,
        spent_ns: 0,
    }
}

async fn next_completion(
    futures: &FutureTable,
    owner: InstanceId,
    wake: &tokio::sync::Notify,
) -> (u64, ResultRecord) {
    loop {
        if let Some(completion) = futures.take_ready(owner) {
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
            .alloc(owner, 1, "sleep", 4)
            .expect("the test dispatch must have a free future slot")
    }

    #[test]
    fn coalesced_notifications_do_not_hide_a_second_ready_completion() {
        let table = FutureTable::new();
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
        let table = FutureTable::new();
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
