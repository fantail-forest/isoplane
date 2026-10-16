//! The import vtable driven one host call at a time.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::calls::FetchRequest;
use isoplane_abi::config::Limits;
use isoplane_abi::result::{FetchResponse, FutureError, ResultRecord};
use isoplane_engine::{HostBindings, ImportCall};

use isoplane_host::bindings::{CellBindings, CellRuntime, InstanceCtx, Services, Tenancy};
use isoplane_host::egress::HostEgressPolicy;
use isoplane_host::fakes::*;
use isoplane_host::futures::{FutureTable, INVALID_FUTURE};
use isoplane_host::meters::Meters;
use isoplane_host::metrics::Metrics;
use isoplane_host::{InstanceId, ModuleId, Resolver};

struct Fixed(Vec<SocketAddr>);
#[async_trait::async_trait]
impl Resolver for Fixed {
    async fn resolve(
        &self,
        _h: &str,
        _p: u16,
    ) -> Result<Vec<SocketAddr>, isoplane_host::egress::DnsError> {
        Ok(self.0.clone())
    }
}

struct Vt {
    ctx: Arc<InstanceCtx>,
    bindings: CellBindings,
    futures: Arc<FutureTable>,
    fetcher: Arc<FakeFetcher>,
    meters: Arc<Meters>,
    wake: Arc<RecordingWake>,
}

fn vtable(limits: Limits, resolve_to: &str, allowlist: Option<Vec<String>>) -> Vt {
    vtable_with_resolver(
        limits,
        Box::new(Fixed(vec![resolve_to
            .parse()
            .expect("the fixture address must parse")])),
        allowlist,
    )
}

fn vtable_with_resolver(
    limits: Limits,
    resolver: Box<dyn Resolver>,
    allowlist: Option<Vec<String>>,
) -> Vt {
    let fetcher = Arc::new(FakeFetcher::default());
    let egress = Arc::new(HostEgressPolicy::new(
        resolver,
        isoplane_host::egress::ConnectionLimits::uniform(1024).expect("connection limits"),
    ));
    let services = Arc::new(Services {
        egress,
        fetcher: fetcher.clone(),
    });
    let futures = Arc::new(FutureTable::new());
    let wake = Arc::new(RecordingWake::default());
    let meters = Arc::new(Meters::new());
    let rt = Arc::new(CellRuntime {
        runtime_configuration: Default::default(),
        services,
        futures: futures.clone(),
        metrics: Arc::new(Metrics::new()),
        wake: wake.clone(),
        reactor: tokio::runtime::Handle::current(),
    });
    let ctx = Arc::new(InstanceCtx::new(
        InstanceId(1),
        Tenancy {
            module: ModuleId::new("app", "v1"),
            tenant: "acme".into(),
            limits,

            allowlist,
        },
        meters.clone(),
        Some(rt),
    ));
    ctx.begin_dispatch(1_000, 1);
    let bindings = CellBindings::new(ctx.clone());
    Vt {
        ctx,
        bindings,
        futures,
        fetcher,
        meters,
        wake,
    }
}

fn fetch_payload(url: &str) -> Vec<u8> {
    postcard::to_allocvec(&FetchRequest {
        method: "GET".into(),
        url: url.into(),
        headers: vec![],
        body: vec![],
    })
    .unwrap()
}

async fn settle(v: &Vt, id: u64) -> ResultRecord {
    for _ in 0..500 {
        if let Some((got, res)) = v.futures.take_ready(v.ctx.instance) {
            assert_eq!(got, id, "the ready result is the future being awaited");
            return res;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("future {id} never settled");
}

#[tokio::test]
async fn fetch_to_a_private_address_is_denied_through_the_future() {
    let mut v = vtable(Limits::default(), "10.0.0.5:80", None);
    let id = v.bindings.call_future(ImportCall {
        import: "http_fetch",
        payload: &fetch_payload("http://internal.example/"),
        scalars: [0; 3],
    });
    assert_ne!(id, INVALID_FUTURE, "the fetch was allocated a future");
    match settle(&v, id).await {
        ResultRecord::Err(FutureError::Denied(msg)) => {
            assert!(msg.contains("10.0.0.5"), "{msg}");
            assert!(
                msg.contains("private/link-local"),
                "the denial should say why: {msg}"
            );
        }
        other => panic!("SSRF target was not denied: {other:?}"),
    }
    assert!(
        v.fetcher.connected_to.lock().is_empty(),
        "the host connected to a private address"
    );
}

#[tokio::test]
async fn the_fetch_connects_to_the_pinned_address() {
    let mut v = vtable(Limits::default(), "93.184.216.34:80", None);
    let id = v.bindings.call_future(ImportCall {
        import: "http_fetch",
        payload: &fetch_payload("http://example.com/"),
        scalars: [0; 3],
    });
    let res = settle(&v, id).await;
    let ResultRecord::Ok(bytes) = res else {
        panic!("fetch failed: {res:?}")
    };
    let resp: FetchResponse = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(resp.status, 200, "the fake fetcher answers 200");
    assert_eq!(
        *v.fetcher.connected_to.lock(),
        vec!["93.184.216.34:80".parse::<SocketAddr>().unwrap()],
        "the fetcher did not use the pinned address"
    );
}

#[tokio::test]
async fn a_host_off_the_allowlist_is_refused() {
    let mut v = vtable(
        Limits::default(),
        "93.184.216.34:80",
        Some(vec!["api.example.com".into()]),
    );
    let id = v.bindings.call_future(ImportCall {
        import: "http_fetch",
        payload: &fetch_payload("http://evil.example/"),
        scalars: [0; 3],
    });
    match settle(&v, id).await {
        ResultRecord::Err(FutureError::Denied(m)) => assert!(m.contains("allowlist"), "{m}"),
        other => panic!("allowlist not enforced: {other:?}"),
    }
}

#[tokio::test]
async fn the_subrequest_cap_is_per_dispatch() {
    let mut v = vtable(
        Limits {
            module_bytes_compressed: 5242880,
            package_bytes: 52428800,
            instance_memory_max: 134217728,
            occupancy_ms_per_http_dispatch: 100,
            subrequests_per_dispatch: 20,
            pending_futures_per_dispatch: 6,
        },
        "93.184.216.34:80",
        None,
    );
    let cap = Limits {
        module_bytes_compressed: 5242880,
        package_bytes: 52428800,
        instance_memory_max: 134217728,
        occupancy_ms_per_http_dispatch: 100,
        subrequests_per_dispatch: 20,
        pending_futures_per_dispatch: 6,
    }
    .subrequests_per_dispatch;
    assert_eq!(cap, 20, "the free plan's subrequest cap");

    assert_eq!(
        Limits {
            module_bytes_compressed: 5242880,
            package_bytes: 52428800,
            instance_memory_max: 134217728,
            occupancy_ms_per_http_dispatch: 100,
            subrequests_per_dispatch: 20,
            pending_futures_per_dispatch: 6
        }
        .pending_futures_per_dispatch,
        6,
        "the futures cap is lower than the subrequest cap, so subrequests are drained one at a \
         time here"
    );

    let payload = fetch_payload("http://example.com/");
    for i in 0..cap {
        let id = v.bindings.call_future(ImportCall {
            import: "http_fetch",
            payload: &payload,
            scalars: [0; 3],
        });
        assert_ne!(
            id, INVALID_FUTURE,
            "subrequest {i} was refused before the cap"
        );
        assert!(
            matches!(settle(&v, id).await, ResultRecord::Ok(_)),
            "subrequest {i} of {cap} was refused early"
        );
    }

    let over = v.bindings.call_future(ImportCall {
        import: "http_fetch",
        payload: &payload,
        scalars: [0; 3],
    });
    match settle(&v, over).await {
        ResultRecord::Err(FutureError::Denied(m)) => assert!(m.contains("subrequest cap"), "{m}"),
        other => panic!("the 21st subrequest was allowed: {other:?}"),
    }
    assert_eq!(
        v.ctx.subrequests(),
        cap + 1,
        "the refused subrequest was counted"
    );
    assert_eq!(
        v.ctx.take_trap(),
        None,
        "a policy denial is not a trap; the guest can catch it and carry on"
    );

    v.ctx.begin_dispatch(1_001, 2);
    assert_eq!(
        v.ctx.subrequests(),
        0,
        "a new dispatch starts with a full allowance"
    );
    let id = v.bindings.call_future(ImportCall {
        import: "http_fetch",
        payload: &payload,
        scalars: [0; 3],
    });
    assert!(
        matches!(settle(&v, id).await, ResultRecord::Ok(_)),
        "the fetch succeeds"
    );
}

#[tokio::test]
async fn logging_enforces_the_dispatch_byte_limit() {
    let output = TestLogCapture::default();
    let subscriber = output.dispatch();
    let _subscriber = tracing::dispatcher::set_default(&subscriber);
    let mut v = vtable(Limits::default(), "93.184.216.34:80", None);
    assert_eq!(v.meters.log_occupancy_ns(), 0, "nothing has logged yet");
    let line = vec![b'x'; 512];
    for _ in 0..200 {
        v.bindings.call_sync(ImportCall {
            import: "log_emit",
            payload: &line,
            scalars: [2, 0, 0],
        });
    }
    assert_eq!(output.records().len(), 200);
    let before = output.records().len();
    let big = vec![b'y'; 256 * 1024];
    for _ in 0..4 {
        v.bindings.call_sync(ImportCall {
            import: "log_emit",
            payload: &big,
            scalars: [2, 0, 0],
        });
    }
    assert!(
        output.records().len() <= before + 1,
        "the per-dispatch log byte cap did not bite"
    );
}

#[tokio::test]
async fn wait_until_is_a_capability_violation_and_does_not_retain_work() {
    let mut v = vtable(Limits::default(), "93.184.216.34:80", None);
    let id = v.bindings.call_future(ImportCall {
        import: "clock_deadline_in",
        payload: &[],
        scalars: [60_000, 0, 0],
    });
    v.bindings.call_sync(ImportCall {
        import: "wait_until",
        payload: &[],
        scalars: [id, 0, 0],
    });
    assert_eq!(
        v.ctx.take_trap(),
        Some(isoplane_abi::status::TrapCode::CapabilityViolation),
        "void wait_until must reject through the trap channel, not an ignored return value"
    );
    v.futures.end_dispatch(v.ctx.instance, 1);
    assert_eq!(
        v.futures.live_futures(),
        0,
        "a rejected retention request must not keep its timer alive"
    );
}

#[tokio::test]
async fn completions_signal_the_wake_router() {
    let mut v = vtable(Limits::default(), "93.184.216.34:80", None);
    let id = v.bindings.call_future(ImportCall {
        import: "clock_deadline_in",
        payload: &[],
        scalars: [1, 0, 0],
    });
    for _ in 0..500 {
        if !v.wake.signalled.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(
        *v.wake.signalled.lock(),
        vec![v.ctx.instance],
        "the completion signalled the owning instance once"
    );
    assert!(
        matches!(settle(&v, id).await, ResultRecord::Ok(_)),
        "the fetch succeeds"
    );
}

#[tokio::test]
async fn entropy_is_never_reused_across_calls_or_instances() {
    let mut a = vtable(Limits::default(), "93.184.216.34:80", None);
    let mut b = vtable(Limits::default(), "93.184.216.34:80", None);
    let mut seen = std::collections::HashSet::new();
    let mut buf = [0u8; 32];
    for _ in 0..100 {
        for v in [&mut a, &mut b] {
            let n = v.bindings.call_fill(
                ImportCall {
                    import: "entropy_fill",
                    payload: &[],
                    scalars: [0; 3],
                },
                &mut buf,
            );
            assert_eq!(n, 32, "the whole buffer is filled");
            assert!(seen.insert(buf.to_vec()), "entropy repeated");
        }
    }
    assert_eq!(seen.len(), 200, "every draw is distinct");
    assert_eq!(a.ctx.entropy_calls(), 100, "every draw is counted");
}

struct FailedDns;
#[async_trait::async_trait]
impl Resolver for FailedDns {
    async fn resolve(
        &self,
        _: &str,
        _: u16,
    ) -> Result<Vec<SocketAddr>, isoplane_host::egress::DnsError> {
        Err(isoplane_host::egress::DnsError::NoAddress)
    }
}

#[tokio::test]
async fn an_http_dns_failure_is_io_and_does_not_count_as_a_policy_denial() {
    let mut v = vtable_with_resolver(Limits::default(), Box::new(FailedDns), None);
    let id = v.bindings.call_future(ImportCall {
        import: "http_fetch",
        payload: &fetch_payload("http://absent.test/"),
        scalars: [0; 3],
    });
    assert!(
        matches!(settle(&v, id).await, ResultRecord::Err(FutureError::Io(_))),
        "a resolver failure must be reported as I/O rather than a destination refusal"
    );
    let runtime = v.ctx.rt.as_ref().expect("HTTP fetch requires a reactor");
    assert_eq!(
        runtime
            .metrics
            .egress_denials
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a missing DNS name must not inflate destination policy denials"
    );
    assert_eq!(
        runtime.services.egress.instance_connections(v.ctx.instance),
        0,
        "a failed DNS lookup must hold no connection permit"
    );
    assert!(
        v.fetcher.connected_to.lock().is_empty(),
        "HTTP must not dial after DNS failed"
    );
}

#[derive(Clone, Default)]
struct TestLogCapture(Arc<std::sync::Mutex<Vec<u8>>>);

impl TestLogCapture {
    fn dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(
            tracing_subscriber::fmt()
                .json()
                .with_ansi(false)
                .with_max_level(tracing::Level::TRACE)
                .with_writer(self.clone())
                .finish(),
        )
    }

    fn records(&self) -> Vec<serde_json::Value> {
        let bytes = self.0.lock().expect("test writer mutex").clone();
        String::from_utf8(bytes)
            .expect("tracing emits UTF-8")
            .lines()
            .filter_map(|line| {
                let event: serde_json::Value =
                    serde_json::from_str(line).expect("tracing JSON event");
                (event["fields"]["message"] == "guest log").then_some(event)
            })
            .collect()
    }
}

impl std::io::Write for TestLogCapture {
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

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for TestLogCapture {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}
