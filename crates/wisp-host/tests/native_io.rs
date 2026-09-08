//! NativeAOT readiness across destroyed Stores using the real host reactor.

use std::sync::Arc;
use std::time::Duration;
use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
use wisp_engine::{Completion, CpuBudget, EngineConfig, InstanceOps, WispEngine};
use wisp_host::bindings::{CellBindings, CellRuntime, InstanceCtx, Services, Tenancy, WakeSink};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::FakeFetcher;
use wisp_host::futures::FutureTable;
use wisp_host::meters::Meters;
use wisp_host::metrics::Metrics;
use wisp_host::{InstanceId, ModuleId, Resolver};

struct NoDns;
impl Resolver for NoDns {
    fn resolve(&self, _: &str, _: u16) -> Result<Vec<std::net::SocketAddr>, String> {
        panic!("a managed timer must not request DNS")
    }
}

struct Wake(tokio::sync::Notify);
impl WakeSink for Wake {
    fn signal(&self, _: InstanceId) {
        self.0.notify_one();
    }
}

#[tokio::test(start_paused = true)]
async fn dropping_a_native_timer_removes_its_host_work_without_entering_managed_code() {
    use wisp_abi::native_io::{DurationNanos, Request, Response};
    use wisp_engine::HostBindings;
    let owner = InstanceId::alloc();
    let (context, futures, _) = context(owner, HostEgressPolicy::new(Box::new(NoDns), 1, 1 << 20));
    context.begin_slice(1, 1);
    let mut bindings = CellBindings::new(context);
    let Response::Handle(timer) = bindings
        .native_io(Request::Timer(DurationNanos(100_000_000)))
        .expect("the host must create a bounded timer")
    else {
        panic!("timer creation must return a handle")
    };
    assert_eq!(
        futures.pending_total(owner),
        1,
        "the timer must register host-owned work"
    );
    tokio::task::yield_now().await;
    bindings
        .native_io(Request::Drop(timer))
        .expect("the owner must dispose its timer without a guest invocation");
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        futures.pending_total(owner),
        0,
        "disposing the timer must remove pending work"
    );
    assert!(
        !futures.has_ready(owner),
        "disposed timers must not deliver late wake notifications"
    );
}

#[tokio::test(start_paused = true)]
async fn a_managed_timer_wakes_on_another_worker_after_its_store_is_destroyed() {
    LogCapture::global();
    let wasm = wisp_fixtures::dotnet("wisp_guest_dotnet_timer");
    let engine = WispEngine::new(EngineConfig {
        max_instances: 2,
        max_memory_bytes: 256 << 20,
        guard_size_bytes: 64 << 20,
        ..EngineConfig::default()
    })
    .expect("the timer fixture engine must initialize");
    let template = engine
        .load_template(&wasm)
        .expect("the native timer fixture must load");
    let owner = InstanceId::alloc();
    let (context, futures, wake) =
        context(owner, HostEgressPolicy::new(Box::new(NoDns), 1, 1 << 20));
    context.begin_slice(1, 1);
    let mut instance = engine
        .activate(&template, CellBindings::boxed(context.clone()))
        .expect("the native timer fixture must activate");
    let outcome = instance.dispatch(
        &DispatchRecord {
            dispatch_id: 1,
            kind: DispatchKind::Http(HttpRequest {
                method: "GET".into(),
                url: "https://timer.test/".into(),
                path: "/".into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            }),
            trace: TraceContext::default(),
            cancel_future: 0,
            cpu_budget_ms: 30_000,
            now_ms: 1,
        },
        budget(),
    );
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "Task.Delay must return to the host with a pending timer: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
    assert_eq!(
        futures.pending_total(owner),
        1,
        "one managed timer must own one host wake"
    );
    let image = instance
        .capture_mutable_state()
        .expect("the returned timer guest must be capturable");
    let memory = instance.memory().to_vec();
    drop(instance);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(60), wake.0.notified())
        .await
        .expect("the host timer must signal readiness before the test deadline");
    let (future, result) = futures
        .take_ready(owner)
        .expect("the advanced host timer must produce its wake");
    context.begin_slice(101, 1);
    let completion = std::thread::spawn(move || {
        let mut restored = engine
            .activate(&template, CellBindings::boxed(context))
            .expect("the timer guest must activate on another worker");
        restored
            .restore_mutable_state(&image)
            .expect("the timer globals must restore");
        restored.memory().copy_from_slice(&memory);
        restored.wake(future, &result, budget()).completion
    })
    .join()
    .expect("the timer worker must not panic");
    assert_eq!(
        completion,
        Completion::Done,
        "the managed timer continuation must run after Store destruction"
    );
}

fn budget() -> CpuBudget {
    CpuBudget {
        cpu_ms: 30_000,
        wall_ms_cap: 60_000,
        spent_ns: 0,
    }
}

fn context(
    owner: InstanceId,
    policy: HostEgressPolicy,
) -> (Arc<InstanceCtx>, Arc<FutureTable>, Arc<Wake>) {
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
            module: ModuleId::new("native-timer", "v1"),
            tenant: "timer-test".into(),
            limits: Limits::default(),

            lifecycle: HandlerLifecycle::Reuse,
            allowlist: None,
        },
        Arc::new(Meters::new()),
        Some(runtime),
    ));
    (context, futures, wake)
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
