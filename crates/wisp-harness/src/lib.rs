//! A harness for tests that drive a managed guest through the real reactor:
//! a cell runtime with in-memory services and an injected egress policy, and
//! a guest whose every host return destroys its Store and whose every wake
//! restores it on another thread, exercising returned-state reconstruction.
//!
//! Test support only: it panics on every failure because its callers are
//! tests. A dev-dependency only; nothing here is linked into a daemon.

use std::sync::Arc;
use std::time::Duration;

use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_abi::result::ResultRecord;
use wisp_engine::{
    Completion, CpuBudget, DispatchOutcome, EngineConfig, Instance, InstanceImage, InstanceOps,
    Template, WispEngine,
};

use wisp_host::bindings::{CellBindings, CellRuntime, InstanceCtx, Services, Tenancy, WakeSink};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::FakeFetcher;
use wisp_host::futures::FutureTable;
use wisp_host::meters::Meters;
use wisp_host::metrics::Metrics;
use wisp_host::{InstanceId, ModuleId};

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
}

impl Reactor {
    /// A reactor on the current tokio runtime for `owner`, whose outbound
    /// connections and lookups go through `policy`.
    pub fn new(owner: InstanceId, policy: HostEgressPolicy) -> Self {
        LogCapture::global();
        let futures = Arc::new(FutureTable::new());
        let wake = Arc::new(Wake(tokio::sync::Notify::new()));
        let runtime = Arc::new(CellRuntime {
            services: Arc::new(Services {
                egress: Arc::new(policy),
                fetcher: Arc::new(FakeFetcher::default()),
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

/// A guest that returns to the host on an empty stack: parked or done, its
/// Store is destroyed, and the next call restores it on another thread.
pub struct ReturnedGuest {
    engine: WispEngine,
    template: Template,
    context: Arc<InstanceCtx>,
    snapshot: Option<(InstanceImage, Vec<u8>)>,
}

impl ReturnedGuest {
    /// Loads `module` for `reactor`'s instance.
    pub fn new(module: &[u8], reactor: &Reactor) -> Self {
        let engine = WispEngine::new(EngineConfig {
            max_instances: 2,
            max_memory_bytes: 256 << 20,
            guard_size_bytes: 64 << 20,
            ..EngineConfig::default()
        })
        .expect("the returned-guest engine must initialize");
        let template = engine
            .load_template(module)
            .unwrap_or_else(|error| panic!("the guest module must load: {error:#}"));
        Self {
            engine,
            template,
            context: reactor.context.clone(),
            snapshot: None,
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

    /// Waits for the reactor to signal readiness, then delivers the ready
    /// completion to the guest restored on another thread.
    pub async fn wake(&mut self, reactor: &Reactor) -> DispatchOutcome {
        tokio::time::timeout(WAKE_DEADLINE, reactor.wake.0.notified())
            .await
            .expect("the reactor must signal readiness before the test deadline");
        let (future, result) = reactor
            .futures
            .take_ready(self.context.instance)
            .expect("a host wake must carry its owner's completion");
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
        let wisp_abi::status::PollStatus::Done { response } = outcome.status else {
            panic!(
                "{path} must complete: {outcome:?}; logs: {:?}",
                self.context.emitted_logs()
            );
        };
        String::from_utf8_lossy(
            &self
                .context
                .responses
                .lock()
                .take(response)
                .unwrap_or_else(|| panic!("{path} must produce a response"))
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

    impl EmittedGuestLogs for wisp_host::bindings::InstanceCtx {
        fn emitted_logs(&self) -> Vec<GuestLog> {
            LogCapture::global().for_instance(self.instance)
        }
    }
}

pub use logging::{EmittedGuestLogs, GuestLog, LogCapture};
