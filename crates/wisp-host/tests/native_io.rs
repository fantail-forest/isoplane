//! NativeAOT readiness across destroyed Stores using the real host reactor.

use std::sync::Arc;
use std::time::Duration;
use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_engine::Completion;
#[path = "common/connection_factory.rs"]
mod connection_factory;
#[path = "common/native_io.rs"]
mod harness;
use harness::ReturnedGuest;
use wisp_host::bindings::{CellBindings, CellRuntime, InstanceCtx, Services, Tenancy, WakeSink};
use wisp_host::egress::HostEgressPolicy;
use wisp_host::fakes::FakeFetcher;
use wisp_host::futures::FutureTable;
use wisp_host::meters::Meters;
use wisp_host::metrics::Metrics;
use wisp_host::{InstanceId, ModuleId, Resolver};

struct NoDns;
#[async_trait::async_trait]
impl Resolver for NoDns {
    async fn resolve(
        &self,
        _: &str,
        _: u16,
    ) -> Result<Vec<std::net::SocketAddr>, wisp_host::egress::DnsError> {
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
async fn a_managed_timer_wakes_on_another_worker_after_its_store_is_destroyed() {
    LogCapture::global();
    let owner = InstanceId::alloc();
    let (context, futures, wake) =
        context(owner, HostEgressPolicy::new(Box::new(NoDns), 1, 1 << 20));
    context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new("wisp_guest_dotnet_timer", context.clone());
    let outcome = guest.start("/");
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "Task.Delay must park: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
    assert_eq!(
        futures.pending_total(owner),
        1,
        "one managed timer must own one host wake"
    );
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    context.begin_slice(101, 1);
    let outcome = guest.wake(&wake.0).await;
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the managed timer must finish after Store destruction: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
}

#[tokio::test(start_paused = true)]
async fn an_abandoned_managed_timer_cannot_run_during_a_later_request() {
    LogCapture::global();
    let owner = InstanceId::alloc();
    let (context, futures, _) = context(owner, HostEgressPolicy::new(Box::new(NoDns), 1, 1 << 20));
    context.begin_dispatch(1, 1);
    let mut guest = ReturnedGuest::new("wisp_guest_dotnet_timer", context.clone());
    let outcome = guest.start("/abandon");
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the request must finish without awaiting its timer: {outcome:?}"
    );
    assert_eq!(
        futures.pending_total(owner),
        1,
        "the abandoned managed delay must register a real host timer"
    );
    futures.end_dispatch(owner, 1);
    assert_eq!(
        futures.live_futures(),
        0,
        "dispatch completion must cancel the timer's host future"
    );
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    // Observe twice: a callback pumped after response creation must not escape detection.
    for dispatch in 2..=3 {
        context.begin_dispatch(101, dispatch);
        let outcome = guest.start("/observe");
        let wisp_abi::status::PollStatus::Done { response } = outcome.status else {
            panic!(
                "the later request must complete: {outcome:?}; logs: {:?}",
                context.emitted_logs()
            );
        };
        let body = context
            .responses
            .lock()
            .take(response)
            .expect("the timer probe must produce a response")
            .body();
        assert_eq!(
            body,
            format!("{dispatch}:0").as_bytes(),
            "the reused guest lost state or ran an abandoned timer callback"
        );
        futures.end_dispatch(owner, dispatch);
    }
}

struct ExactPeer(std::net::SocketAddr);
impl wisp_host::egress::DestinationPolicy for ExactPeer {
    fn authorize(&self, peer: std::net::SocketAddr) -> Result<(), wisp_host::egress::DenyReason> {
        if peer == self.0 {
            Ok(())
        } else {
            Err(wisp_host::egress::DenyReason::NotAllowlisted(
                peer.to_string(),
            ))
        }
    }
}

#[tokio::test]
async fn a_managed_connection_survives_store_destruction_and_disposal_closes_its_real_peer() {
    socket_round_trip("wisp_guest_dotnet_connect", None).await;
}

#[tokio::test]
async fn a_managed_network_stream_reads_and_writes_after_store_destruction() {
    socket_round_trip("wisp_guest_dotnet_stream", Some(b"wisp")).await;
}

#[tokio::test]
async fn a_managed_half_closed_socket_retains_its_read_direction_after_restoring() {
    socket_transfer(
        "wisp_guest_dotnet_stream",
        Some(b"wisp"),
        true,
        std::net::Ipv4Addr::LOCALHOST.into(),
    )
    .await;
}

#[tokio::test]
async fn an_ipv6_managed_network_stream_survives_store_destruction() {
    socket_transfer(
        "wisp_guest_dotnet_stream",
        Some(b"wisp"),
        false,
        std::net::Ipv6Addr::LOCALHOST.into(),
    )
    .await;
}

async fn socket_round_trip(fixture: &str, payload: Option<&[u8]>) {
    socket_transfer(
        fixture,
        payload,
        false,
        std::net::Ipv4Addr::LOCALHOST.into(),
    )
    .await;
}

async fn socket_transfer(
    fixture: &str,
    payload: Option<&[u8]>,
    half_close: bool,
    ip: std::net::IpAddr,
) {
    LogCapture::global();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(std::net::SocketAddr::new(ip, 0))
        .await
        .expect("the owned test listener must bind");
    let peer = listener
        .local_addr()
        .expect("the test listener must have an endpoint");
    let connections = Arc::new(connection_factory::RecordingConnections::default());
    let policy = HostEgressPolicy::new(Box::new(NoDns), 1, 1 << 20)
        .with_connections(connections.clone())
        .with_destinations(Arc::new(ExactPeer(peer)));
    let owner = InstanceId::alloc();
    let (context, futures, wake) = context(owner, policy);
    context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new(fixture, context.clone());
    let suffix = if half_close { "/half" } else { "" };
    let outcome = guest.start(&format!("/{}/{}{suffix}", peer.ip(), peer.port()));
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "ConnectAsync must park before the reactor runs: {outcome:?}; pending: {}; logs: {:?}",
        futures.pending_total(owner),
        context.emitted_logs()
    );
    let (mut server, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept())
        .await
        .expect("the connect must reach its owned listener before the test deadline")
        .expect("the listener must accept the native connection");
    let phases = if payload.is_some() { 2 } else { 1 };
    for phase in 0..phases {
        let outcome = guest.wake(&wake.0).await;
        let expected = if phase + 1 == phases {
            Completion::Done
        } else {
            Completion::Parked
        };
        assert_eq!(outcome.completion, expected,
            "native I/O phase {phase} must preserve its continuation after Store destruction: {outcome:?}; logs: {:?}", context.emitted_logs());
        if phase == 0 {
            if let Some(payload) = payload {
                if half_close {
                    let mut byte = [0];
                    let count =
                        tokio::time::timeout(Duration::from_secs(60), server.read(&mut byte))
                            .await
                            .expect("the send shutdown must reach the peer before the deadline")
                            .expect("the peer must observe send-direction EOF");
                    assert_eq!(
                        count, 0,
                        "Socket.Shutdown(Send) must close the peer's input direction"
                    );
                }
                server
                    .write_all(&payload[..1])
                    .await
                    .expect("the owned peer must deliver the first partial byte");
                let partial = guest.wake(&wake.0).await;
                assert_eq!(partial.completion, Completion::Parked,
                    "ReadExactlyAsync must retain partial input across another destroyed Store: {partial:?}; logs: {:?}", context.emitted_logs());
                server
                    .write_all(&payload[1..])
                    .await
                    .expect("the owned peer must deliver the remaining test payload");
            }
        }
    }
    if let Some(payload) = payload {
        if half_close {
            assert_eq!(
                context
                    .responses
                    .lock()
                    .get(1)
                    .expect("the half-close fixture must respond")
                    .body(),
                b"77697370",
                "the receive direction must preserve the peer's bytes after send shutdown"
            );
        } else {
            let mut received = vec![0; payload.len()];
            tokio::time::timeout(Duration::from_secs(60), server.read_exact(&mut received))
                .await
                .expect("the echo must arrive before the test deadline")
                .expect("NetworkStream must send every payload byte");
            assert_eq!(
                received, payload,
                "NetworkStream must preserve bytes across both pending I/O boundaries"
            );
        }
    }
    assert_eq!(*connections.calls.lock(), vec![(wisp_host::egress::ConnectionOwner::new(context.tenant.clone(), owner).expect("the dispatch establishes the connection owner"), peer)],
        "managed TCP must open exactly one connection through the injected provider with its tenant and instance");
    let mut byte = [0];
    let read = tokio::time::timeout(Duration::from_secs(60), server.read(&mut byte))
        .await
        .expect("socket disposal must close the real peer before the test deadline")
        .expect("the real peer must observe an orderly close");
    assert_eq!(
        read, 0,
        "disposing the managed socket must close its host descriptor"
    );
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
        .native_io(Request::Drop {
            handle: timer,
            kind: wisp_abi::native_io::ResourceKind::Pollable,
        })
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

struct PublicDns;
#[async_trait::async_trait]
impl Resolver for PublicDns {
    async fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, wisp_host::egress::DnsError> {
        assert_eq!(
            host, "owned.test",
            "the managed DNS name must reach the host unchanged"
        );
        Ok(vec![std::net::SocketAddr::from(([93, 184, 216, 34], port))])
    }
}
#[tokio::test]
async fn managed_dns_finishes_after_its_store_is_destroyed() {
    LogCapture::global();
    let owner = InstanceId::alloc();
    let (context, futures, wake) = context(
        owner,
        HostEgressPolicy::new(Box::new(PublicDns), 1, 1 << 20),
    );
    context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new("wisp_guest_dotnet_dns", context.clone());
    let outcome = guest.start("/owned.test");
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "Dns.GetHostAddressesAsync must return with pending DNS: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
    assert_eq!(
        futures.pending_total(owner),
        1,
        "one managed lookup must own one host wake"
    );
    let outcome = guest.wake(&wake.0).await;
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "managed DNS must finish after Store destruction: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
    assert_eq!(
        context
            .responses
            .lock()
            .get(1)
            .expect("the DNS handler must produce a response")
            .body(),
        b"93.184.216.34",
        "the restored managed DNS await must preserve the actual answer"
    );
}

struct PendingDns {
    entered: Arc<tokio::sync::Notify>,
    stopped: Arc<tokio::sync::Notify>,
}

struct LookupStopped(Arc<tokio::sync::Notify>);
impl Drop for LookupStopped {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[async_trait::async_trait]
impl Resolver for PendingDns {
    async fn resolve(
        &self,
        _: &str,
        _: u16,
    ) -> Result<Vec<std::net::SocketAddr>, wisp_host::egress::DnsError> {
        let _stopped = LookupStopped(self.stopped.clone());
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn managed_dns_cancellation_after_store_destruction_releases_host_work() {
    LogCapture::global();
    let entered = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::new(tokio::sync::Notify::new());
    let owner = InstanceId::alloc();
    let (context, futures, wake) = context(
        owner,
        HostEgressPolicy::new(
            Box::new(PendingDns {
                entered: entered.clone(),
                stopped: stopped.clone(),
            }),
            1,
            1 << 20,
        ),
    );
    context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new("wisp_guest_dotnet_dns", context.clone());
    let outcome = guest.start("/cancel.test");
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "DNS cancellation must wait on its host-owned timer: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
    tokio::time::timeout(Duration::from_secs(60), entered.notified())
        .await
        .expect("DNS must start before the test deadline");
    assert_eq!(
        futures.pending_total(owner),
        2,
        "the pending lookup and cancellation timer must both be host-owned"
    );
    tokio::time::advance(Duration::from_millis(100)).await;
    context.begin_slice(101, 1);
    let outcome = guest.wake(&wake.0).await;
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "managed DNS cancellation must finish after Store destruction: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
    tokio::time::timeout(Duration::from_secs(60), stopped.notified())
        .await
        .expect("managed cancellation must drop the active host DNS lookup");
    assert_eq!(
        futures.pending_total(owner),
        0,
        "managed cancellation must reclaim every pending DNS and timer future"
    );
    assert!(
        !futures.has_ready(owner),
        "cancelled DNS must leave no late guest wake"
    );
    assert_eq!(
        context
            .rt
            .as_ref()
            .expect("the test must own a reactor")
            .services
            .egress
            .connections(),
        0,
        "cancelled name resolution must retain no connection quota"
    );
    assert_eq!(
        context
            .responses
            .lock()
            .get(1)
            .expect("the cancellation handler must respond")
            .body(),
        b"cancelled",
        "the ordinary DNS task must observe managed cancellation"
    );
}

fn managed_network_refusal(path: &str, expected: &[u8]) {
    LogCapture::global();
    let owner = InstanceId::alloc();
    let (context, futures, _) = context(owner, HostEgressPolicy::new(Box::new(NoDns), 1, 1024));
    context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new("wisp_guest_dotnet_network_refusal", context.clone());
    let outcome = guest.start(path);
    assert_eq!(outcome.completion, Completion::Done,
        "a forbidden managed listener must return a SocketException instead of trapping: {outcome:?}; logs: {:?}", context.emitted_logs());
    assert_eq!(
        context
            .responses
            .lock()
            .get(1)
            .expect("the refusal fixture must report its exception")
            .body(),
        expected,
        "the managed listener must receive the idiomatic networking error for {path}"
    );
    assert_eq!(
        futures.pending_total(owner),
        0,
        "a refused listener must retain no reactor work"
    );
    assert_eq!(
        context
            .rt
            .as_ref()
            .expect("the refusal fixture must own its reactor")
            .services
            .egress
            .connections(),
        0,
        "a refused listener must acquire no host connection quota"
    );
}

#[tokio::test]
async fn a_managed_tcp_listener_receives_access_denied() {
    managed_network_refusal("/tcp-listener", b"AccessDenied");
}
#[tokio::test]
async fn a_managed_tcp_bind_receives_access_denied() {
    managed_network_refusal("/tcp-bind", b"AccessDenied");
}
#[tokio::test]
async fn a_managed_tcp_listen_receives_access_denied() {
    managed_network_refusal("/tcp-listen", b"AccessDenied");
}
#[tokio::test]
async fn a_managed_udp_client_receives_operation_not_supported() {
    managed_network_refusal("/udp-client", b"OperationNotSupported");
}
#[tokio::test]
async fn a_managed_udp_bind_receives_operation_not_supported() {
    managed_network_refusal("/udp-bind", b"OperationNotSupported");
}

#[tokio::test]
async fn a_refused_managed_connection_preserves_its_socket_error_after_restoring() {
    LogCapture::global();
    let reservation = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .expect("the test must reserve its non-listening TCP endpoint");
    reservation
        .bind(&std::net::SocketAddr::from(([127, 0, 0, 1], 0)).into())
        .expect("the non-listening endpoint must bind");
    let peer = reservation
        .local_addr()
        .expect("the reservation must have an endpoint")
        .as_socket()
        .expect("the reservation must be an IP endpoint");
    let policy = HostEgressPolicy::new(Box::new(NoDns), 1, 1024)
        .with_destinations(Arc::new(ExactPeer(peer)));
    let owner = InstanceId::alloc();
    let (context, futures, wake) = context(owner, policy);
    context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new("wisp_guest_dotnet_connect", context.clone());
    let pending = guest.start(&format!("/{}/{}", peer.ip(), peer.port()));
    assert_eq!(
        pending.completion,
        Completion::Parked,
        "ConnectAsync must return before its owned reactor runs"
    );
    let outcome = guest.wake(&wake.0).await;
    assert_eq!(outcome.completion, Completion::Done,
        "the connection refusal must reach the restored managed continuation: {outcome:?}; logs: {:?}", context.emitted_logs());
    assert_eq!(
        context
            .responses
            .lock()
            .get(1)
            .expect("the connection fixture must report its error")
            .body(),
        b"ConnectionRefused",
        "the host must preserve the operating-system connection refusal through SocketException"
    );
    assert_eq!(
        futures.pending_total(owner),
        0,
        "a refused connection must retain no wake"
    );
    assert_eq!(
        context
            .rt
            .as_ref()
            .expect("the connection test must have a reactor")
            .services
            .egress
            .connections(),
        0,
        "a refused connection must release its reserved quota"
    );
}

#[tokio::test]
async fn a_managed_read_cancels_after_restoration_and_releases_its_connection() {
    stream_failure("cancel", b"cancelled").await;
}

#[tokio::test]
async fn a_managed_read_receives_clean_eof_after_restoration() {
    stream_failure("eof", b"eof").await;
}

#[tokio::test]
async fn a_managed_read_receives_a_peer_reset_after_restoration() {
    stream_failure("reset", b"stream-error").await;
}

async fn stream_failure(mode: &str, expected: &[u8]) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the stream failure test must own its peer");
    let peer = listener
        .local_addr()
        .expect("the peer must have an endpoint");
    let policy = HostEgressPolicy::new(Box::new(NoDns), 1, 1024)
        .with_destinations(Arc::new(ExactPeer(peer)));
    let owner = InstanceId::alloc();
    let (context, futures, wake) = context(owner, policy);
    context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new("wisp_guest_dotnet_stream", context.clone());
    let outcome = guest.start(&format!("/{}/{}/{mode}", peer.ip(), peer.port()));
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "the managed stream must return for its connection"
    );
    let (mut server, _) = tokio::time::timeout(Duration::from_secs(60), listener.accept())
        .await
        .expect("the managed connection must arrive before the deadline")
        .expect("the peer must accept");
    let pending = guest.wake(&wake.0).await;
    assert_eq!(
        pending.completion,
        Completion::Parked,
        "the managed read must wait for the controlled peer event"
    );
    match mode {
        "cancel" => {
            assert_eq!(
                futures.pending_total(owner),
                2,
                "the pending read and cancellation timer must each own a wake"
            );
            tokio::time::pause();
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(100)).await;
            context.begin_slice(101, 1);
        }
        "eof" => server
            .shutdown()
            .await
            .expect("the peer must close its send direction"),
        "reset" => {
            socket2::SockRef::from(&server)
                .set_linger(Some(Duration::ZERO))
                .expect("the peer reset must be controlled");
            drop(server);
            let outcome = guest.wake(&wake.0).await;
            assert_stream_failure(&context, &futures, owner, outcome, expected).await;
            return;
        }
        _ => panic!("unknown controlled stream failure: {mode}"),
    }
    let outcome = guest.wake(&wake.0).await;
    if mode == "cancel" {
        tokio::time::resume();
    }
    assert_stream_failure(&context, &futures, owner, outcome, expected).await;
    let mut extra = [0];
    let count = tokio::time::timeout(Duration::from_secs(60), server.read(&mut extra))
        .await
        .expect("managed cleanup must reach the peer before the deadline")
        .expect("the peer must observe cleanup");
    assert_eq!(
        count, 0,
        "managed cancellation or EOF must dispose its host-owned connection"
    );
}

async fn assert_stream_failure(
    context: &InstanceCtx,
    futures: &FutureTable,
    owner: InstanceId,
    outcome: wisp_engine::DispatchOutcome,
    expected: &[u8],
) {
    LogCapture::global();
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "the managed read must receive an exception after restoration: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
    assert_eq!(
        context
            .responses
            .lock()
            .get(1)
            .expect("the managed stream must report its outcome")
            .body(),
        expected,
        "the managed stream must distinguish cancellation, EOF, and reset"
    );
    assert_eq!(
        futures.pending_total(owner),
        0,
        "a terminal stream operation must release every pending wake"
    );
    tokio::time::timeout(Duration::from_secs(60), async {
        while context
            .rt
            .as_ref()
            .expect("the stream must own a reactor")
            .services
            .egress
            .connections()
            != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("aborted stream work must release its connection before the deadline");
    assert_eq!(
        context
            .rt
            .as_ref()
            .expect("the stream must own a reactor")
            .services
            .egress
            .connections(),
        0,
        "a terminal stream operation must release its connection quota"
    );
}

#[tokio::test]
async fn an_unsupported_managed_keepalive_option_receives_operation_not_supported() {
    managed_network_refusal("/keepalive", b"OperationNotSupported");
}
#[tokio::test]
async fn an_unsupported_managed_keepalive_count_receives_operation_not_supported() {
    managed_network_refusal("/keepalive-count", b"OperationNotSupported");
}
#[tokio::test]
async fn an_unsupported_managed_buffer_option_receives_operation_not_supported() {
    managed_network_refusal("/receive-buffer", b"OperationNotSupported");
}

struct FailingDns(wisp_abi::native_io::DnsFailure);
#[async_trait::async_trait]
impl Resolver for FailingDns {
    async fn resolve(
        &self,
        host: &str,
        _: u16,
    ) -> Result<Vec<std::net::SocketAddr>, wisp_host::egress::DnsError> {
        Err(wisp_host::egress::DnsError::Lookup {
            host: host.to_owned(),
            kind: self.0,
            message: "controlled resolver failure".into(),
        })
    }
}

async fn managed_dns_error(resolver: Box<dyn Resolver>, expected: &[u8], denials: u64) {
    LogCapture::global();
    let owner = InstanceId::alloc();
    let (context, futures, wake) = context(owner, HostEgressPolicy::new(resolver, 1, 1024));
    context.begin_slice(1, 1);
    let mut guest = ReturnedGuest::new("wisp_guest_dotnet_dns", context.clone());
    let started = guest.start("/error.test");
    assert_eq!(
        started.completion,
        Completion::Parked,
        "DNS errors must arrive through owned async work: {started:?}"
    );
    let outcome = guest.wake(&wake.0).await;
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "managed DNS must catch its typed SocketException: {outcome:?}; logs: {:?}",
        context.emitted_logs()
    );
    assert_eq!(
        context
            .responses
            .lock()
            .get(1)
            .expect("the DNS fixture must respond")
            .body(),
        expected,
        "the ordinary DNS API must expose the matching SocketErrorCode"
    );
    assert_eq!(
        context
            .rt
            .as_ref()
            .expect("DNS requires a reactor")
            .metrics
            .egress_denials
            .load(std::sync::atomic::Ordering::Relaxed),
        denials,
        "only a destination policy refusal must increment denial accounting"
    );
    assert_eq!(
        context
            .rt
            .as_ref()
            .expect("DNS requires a reactor")
            .services
            .egress
            .connections(),
        0,
        "a failed or denied DNS answer must acquire no outbound connection"
    );
    assert_eq!(
        futures.pending_total(owner),
        0,
        "a failed DNS lookup must release its pending host work"
    );
}

#[tokio::test]
async fn managed_dns_reports_a_missing_name_without_counting_a_denial() {
    managed_dns_error(
        Box::new(FailingDns(wisp_abi::native_io::DnsFailure::NotFound)),
        b"HostNotFound",
        0,
    )
    .await;
}

#[tokio::test]
async fn managed_dns_reports_a_retryable_resolver_failure_without_counting_a_denial() {
    managed_dns_error(
        Box::new(FailingDns(wisp_abi::native_io::DnsFailure::Temporary)),
        b"TryAgain",
        0,
    )
    .await;
}

#[tokio::test]
async fn managed_dns_reports_a_resolver_timeout_without_counting_a_denial() {
    managed_dns_error(
        Box::new(FailingDns(wisp_abi::native_io::DnsFailure::Timeout)),
        b"TimedOut",
        0,
    )
    .await;
}

#[tokio::test]
async fn managed_dns_reports_a_permanent_resolver_failure_without_counting_a_denial() {
    managed_dns_error(
        Box::new(FailingDns(wisp_abi::native_io::DnsFailure::Permanent)),
        b"NoRecovery",
        0,
    )
    .await;
}

struct MixedDns;
#[async_trait::async_trait]
impl Resolver for MixedDns {
    async fn resolve(
        &self,
        _: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, wisp_host::egress::DnsError> {
        Ok(vec![
            std::net::SocketAddr::from(([93, 184, 216, 34], port)),
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        ])
    }
}

#[tokio::test]
async fn managed_dns_refuses_a_mixed_public_private_answer_and_counts_the_policy_denial() {
    managed_dns_error(Box::new(MixedDns), b"AccessDenied", 1).await;
}

#[tokio::test]
async fn a_native_tcp_policy_refusal_counts_once_without_starting_owned_work() {
    use wisp_abi::native_io::{AddressFamily, Error, Request, Response};
    use wisp_engine::HostBindings;
    let owner = InstanceId::alloc();
    let (context, futures, _) = context(owner, HostEgressPolicy::new(Box::new(NoDns), 1, 1024));
    let mut bindings = CellBindings::new(context.clone());
    let Response::Handle(network) = bindings
        .native_io(Request::Network)
        .expect("the instance network must exist")
    else {
        panic!("network acquisition must return its handle")
    };
    let Response::Handle(socket) = bindings
        .native_io(Request::CreateTcp(AddressFamily::V4))
        .expect("socket allocation must succeed")
    else {
        panic!("socket creation must return its handle")
    };
    let result = bindings.native_io(Request::Connect {
        socket,
        network,
        destination: std::net::SocketAddr::from(([127, 0, 0, 1], 1)),
    });
    assert!(
        matches!(result, Err(Error::Denied(_))),
        "native TCP must reject private destinations before dialing"
    );
    let runtime = context
        .rt
        .as_ref()
        .expect("the instance must have its reactor");
    assert_eq!(
        runtime
            .metrics
            .egress_denials
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a refused native connection must record one denial"
    );
    assert_eq!(
        runtime.services.egress.connections(),
        0,
        "a denied native destination must acquire no connection quota"
    );
    assert_eq!(
        futures.pending_total(owner),
        0,
        "a denied native destination must start no pending reactor work"
    );
}

struct PendingConnections {
    started: Arc<tokio::sync::Notify>,
    cancelled: Arc<tokio::sync::Notify>,
}

struct PendingConnectionAttempt(Arc<tokio::sync::Notify>);
impl Drop for PendingConnectionAttempt {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[async_trait::async_trait]
impl wisp_host::egress::ConnectionFactory for PendingConnections {
    async fn connect(
        &self,
        _: &wisp_host::egress::ConnectionOwner,
        _: std::net::SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        let _attempt = PendingConnectionAttempt(self.cancelled.clone());
        self.started.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn dropping_a_socket_cancels_its_pending_connection_provider_and_releases_quota() {
    use wisp_abi::native_io::{AddressFamily, Request, ResourceKind, Response};
    use wisp_engine::HostBindings;
    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let policy = HostEgressPolicy::new(Box::new(NoDns), 1, 1024).with_connections(Arc::new(
        PendingConnections {
            started: started.clone(),
            cancelled: cancelled.clone(),
        },
    ));
    let owner = InstanceId::alloc();
    let (context, futures, _) = context(owner, policy);
    context.begin_slice(1, 1);
    let mut bindings = CellBindings::new(context.clone());
    let (network, socket) = {
        let mut handle = |request| match bindings
            .native_io(request)
            .expect("the owned native resource must allocate")
        {
            Response::Handle(handle) => handle,
            other => panic!("native allocation must return a handle: {other:?}"),
        };
        (
            handle(Request::Network),
            handle(Request::CreateTcp(AddressFamily::V4)),
        )
    };
    bindings
        .native_io(Request::Connect {
            socket,
            network,
            destination: std::net::SocketAddr::from(([93, 184, 216, 34], 443)),
        })
        .expect("the allowed connection must start its provider");
    tokio::time::timeout(Duration::from_secs(60), started.notified())
        .await
        .expect("the connection provider must start before the test deadline");
    assert_eq!(
        futures.pending_total(owner),
        1,
        "the pending connection must own exactly one reactor future"
    );
    bindings
        .native_io(Request::Drop {
            handle: socket,
            kind: ResourceKind::Tcp,
        })
        .expect("dropping the pending socket must succeed");
    assert_eq!(
        futures.pending_total(owner),
        0,
        "socket disposal must remove its pending wake immediately"
    );
    tokio::time::timeout(Duration::from_secs(60), cancelled.notified())
        .await
        .expect("socket disposal must cancel the provider before the test deadline");
    assert_eq!(
        context
            .rt
            .as_ref()
            .expect("the connection has a reactor")
            .services
            .egress
            .connections(),
        0,
        "cancelling a pending connect must release its connection permit"
    );
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
