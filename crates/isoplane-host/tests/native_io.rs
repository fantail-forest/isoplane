//! Managed native I/O across destroyed Stores using Mono and the real host reactor.

use isoplane_engine::Completion;
use std::sync::Arc;
use std::time::Duration;
#[path = "common/connection_factory.rs"]
mod connection_factory;
#[path = "common/native_io.rs"]
#[macro_use]
mod harness;
use harness::{GuestRuntime, Reactor, ReturnedGuest, WAKE_DEADLINE};
use isoplane_host::bindings::CellBindings;
use isoplane_host::{InstanceId, Resolver};
use isoplane_harness::EmittedGuestLogs as _;

struct NoDns;
#[async_trait::async_trait]
impl Resolver for NoDns {
    async fn resolve(
        &self,
        _: &str,
        _: u16,
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        panic!("a managed timer must not request DNS")
    }
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn a_managed_timer_wakes_on_another_worker_after_its_store_is_destroyed(runtime: GuestRuntime) {
        let owner = InstanceId::alloc();
        let reactor = Reactor::with_network(owner, isoplane_harness::Network::with_resolver(Box::new(NoDns)));
        let context = &reactor.context;
        let futures = &reactor.futures;
        context.begin_slice(1, 1);
        let mut guest = ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
        let outcome = guest.start("/timer");
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
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "the managed timer must finish after Store destruction: {outcome:?}; logs: {:?}",
            context.emitted_logs()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_abandoned_managed_timer_cannot_run_during_a_later_request(runtime: GuestRuntime) {
        let owner = InstanceId::alloc();
        let reactor = Reactor::with_network(owner, isoplane_harness::Network::with_resolver(Box::new(NoDns)));
        let context = &reactor.context;
        let futures = &reactor.futures;
        context.begin_dispatch(1, 1);
        let mut guest = ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
        let outcome = guest.start("/timer/abandon");
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
            let outcome = guest.start("/timer/observe");
            let isoplane_abi::status::PollStatus::Done { response } = outcome.status else {
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
}

struct ExactPeer(std::net::SocketAddr);
impl isoplane_host::egress::DestinationPolicy for ExactPeer {
    fn authorize(
        &self,
        peer: std::net::SocketAddr,
    ) -> Result<(), isoplane_host::egress::DenyReason> {
        if peer == self.0 {
            Ok(())
        } else {
            Err(isoplane_host::egress::DenyReason::NotAllowlisted(
                peer.to_string(),
            ))
        }
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_managed_connection_survives_store_destruction_and_disposal_closes_its_real_peer(runtime: GuestRuntime) {
        socket_round_trip("connect", runtime, None).await;
    }

    #[tokio::test]
    async fn a_managed_network_stream_reads_and_writes_after_store_destruction(runtime: GuestRuntime) {
        socket_round_trip("stream", runtime, Some(b"ping")).await;
    }

    #[tokio::test]
    async fn a_managed_half_closed_socket_retains_its_read_direction_after_restoring(runtime: GuestRuntime) {
        socket_transfer(
            "stream",
            runtime,
            Some(b"ping"),
            true,
            std::net::Ipv4Addr::LOCALHOST.into(),
        )
        .await;
    }

    #[tokio::test]
    async fn an_ipv6_managed_network_stream_survives_store_destruction(runtime: GuestRuntime) {
        socket_transfer(
            "stream",
            runtime,
            Some(b"ping"),
            false,
            std::net::Ipv6Addr::LOCALHOST.into(),
        )
        .await;
    }
}

async fn socket_round_trip(probe: &str, runtime: GuestRuntime, payload: Option<&[u8]>) {
    socket_transfer(
        probe,
        runtime,
        payload,
        false,
        std::net::Ipv4Addr::LOCALHOST.into(),
    )
    .await;
}

async fn socket_transfer(
    probe: &str,
    runtime: GuestRuntime,
    payload: Option<&[u8]>,
    half_close: bool,
    ip: std::net::IpAddr,
) {
    isoplane_harness::LogCapture::global();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind(std::net::SocketAddr::new(ip, 0))
        .await
        .expect("the owned test listener must bind");
    let peer = listener
        .local_addr()
        .expect("the test listener must have an endpoint");
    let connections = Arc::new(connection_factory::RecordingConnections::default());
    let policy = isoplane_harness::Network::with_resolver(Box::new(NoDns))
        .with_connections(connections.clone())
        .with_destinations(Arc::new(ExactPeer(peer)));
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(owner, policy);
    let context = &reactor.context;
    let futures = &reactor.futures;
    context.begin_slice(1, 1);
    let mut guest =
        ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
    let suffix = if half_close { "/half" } else { "" };
    let outcome = guest.start(&format!("/{probe}/{}/{}{suffix}", peer.ip(), peer.port()));
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "ConnectAsync must park before the reactor runs: {outcome:?}; pending: {}; logs: {:?}",
        futures.pending_total(owner),
        context.emitted_logs()
    );
    let (mut server, _) = tokio::time::timeout(WAKE_DEADLINE, listener.accept())
        .await
        .expect("the connect must reach its owned listener before the test deadline")
        .expect("the listener must accept the native connection");
    let phases = if payload.is_some() { 2 } else { 1 };
    for phase in 0..phases {
        let outcome = guest.wake(&reactor).await;
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
                    let count = tokio::time::timeout(WAKE_DEADLINE, server.read(&mut byte))
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
                let partial = guest.wake(&reactor).await;
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
                b"70696E67",
                "the receive direction must preserve the peer's bytes after send shutdown"
            );
        } else {
            let mut received = vec![0; payload.len()];
            tokio::time::timeout(WAKE_DEADLINE, server.read_exact(&mut received))
                .await
                .expect("the echo must arrive before the test deadline")
                .expect("NetworkStream must send every payload byte");
            assert_eq!(
                received, payload,
                "NetworkStream must preserve bytes across both pending I/O boundaries"
            );
        }
    }
    assert_eq!(*connections.calls.lock(), vec![(isoplane_host::egress::ConnectionOwner::new(context.tenant.clone(), owner).expect("the dispatch establishes the connection owner"), peer)],
        "managed TCP must open exactly one connection through the injected provider with its tenant and instance");
    let mut byte = [0];
    let read = tokio::time::timeout(WAKE_DEADLINE, server.read(&mut byte))
        .await
        .expect("socket disposal must close the real peer before the test deadline")
        .expect("the real peer must observe an orderly close");
    assert_eq!(
        read, 0,
        "disposing the managed socket must close its host descriptor"
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_a_native_timer_removes_its_host_work_without_entering_managed_code() {
    use isoplane_abi::native_io::{DurationNanos, Request, Response};
    use isoplane_engine::HostBindings;
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(
        owner,
        isoplane_harness::Network::with_resolver(Box::new(NoDns)),
    );
    let context = &reactor.context;
    let futures = &reactor.futures;
    context.begin_slice(1, 1);
    let mut bindings = CellBindings::new(context.clone());
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
            kind: isoplane_abi::native_io::ResourceKind::Pollable,
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
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        assert_eq!(
            host, "owned.test",
            "the managed DNS name must reach the host unchanged"
        );
        Ok(vec![std::net::SocketAddr::from(([93, 184, 216, 34], port))])
    }
}
in_mono_runtime! {
    #[tokio::test]
    async fn managed_dns_finishes_after_its_store_is_destroyed(runtime: GuestRuntime) {
        let owner = InstanceId::alloc();
        let reactor = Reactor::with_network(
            owner,
            isoplane_harness::Network::with_resolver(Box::new(PublicDns)),
        );
        let context = &reactor.context;
        let futures = &reactor.futures;
        context.begin_slice(1, 1);
        let mut guest = ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
        let outcome = guest.start("/dns/owned.test");
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
        let outcome = guest.wake(&reactor).await;
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
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        let _stopped = LookupStopped(self.stopped.clone());
        self.entered.notify_one();
        std::future::pending().await
    }
}

in_mono_runtime! {
    #[tokio::test(start_paused = true)]
    async fn managed_dns_cancellation_after_store_destruction_releases_host_work(runtime: GuestRuntime) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let stopped = Arc::new(tokio::sync::Notify::new());
        let owner = InstanceId::alloc();
        let reactor = Reactor::with_network(
            owner,
            isoplane_harness::Network::with_resolver(Box::new(PendingDns {
                    entered: entered.clone(),
                    stopped: stopped.clone(),
                })),
        );
        let context = &reactor.context;
        let futures = &reactor.futures;
        context.begin_slice(1, 1);
        let mut guest = ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
        let outcome = guest.start("/dns/cancel.test");
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "DNS cancellation must wait on its host-owned timer: {outcome:?}; logs: {:?}",
            context.emitted_logs()
        );
        tokio::time::timeout(WAKE_DEADLINE, entered.notified())
            .await
            .expect("DNS must start before the test deadline");
        assert_eq!(
            futures.pending_total(owner),
            2,
            "the pending lookup and cancellation timer must both be host-owned"
        );
        tokio::time::advance(Duration::from_millis(100)).await;
        context.begin_slice(101, 1);
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "managed DNS cancellation must finish after Store destruction: {outcome:?}; logs: {:?}",
            context.emitted_logs()
        );
        tokio::time::timeout(WAKE_DEADLINE, stopped.notified())
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
            reactor.runtime().services.egress.instance_connections(reactor.context.instance),
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
}

fn managed_network_refusal(runtime: GuestRuntime, path: &str, expected: &[u8]) {
    isoplane_harness::LogCapture::global();
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(
        owner,
        isoplane_harness::Network::with_resolver(Box::new(NoDns)),
    );
    let context = &reactor.context;
    let futures = &reactor.futures;
    context.begin_slice(1, 1);
    let mut guest =
        ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
    let outcome = guest.start(&format!("/refusal{path}"));
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
        reactor
            .runtime()
            .services
            .egress
            .instance_connections(reactor.context.instance),
        0,
        "a refused listener must acquire no host connection quota"
    );
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_managed_tcp_listener_receives_access_denied(runtime: GuestRuntime) {
        managed_network_refusal(runtime, "/tcp-listener", b"AccessDenied");
    }
    #[tokio::test]
    async fn a_managed_tcp_bind_receives_access_denied(runtime: GuestRuntime) {
        managed_network_refusal(runtime, "/tcp-bind", b"AccessDenied");
    }
    #[tokio::test]
    async fn a_managed_tcp_listen_receives_access_denied(runtime: GuestRuntime) {
        managed_network_refusal(runtime, "/tcp-listen", b"AccessDenied");
    }
    #[tokio::test]
    async fn a_managed_udp_client_receives_operation_not_supported(runtime: GuestRuntime) {
        managed_network_refusal(runtime, "/udp-client", b"OperationNotSupported");
    }
    #[tokio::test]
    async fn a_managed_udp_bind_receives_operation_not_supported(runtime: GuestRuntime) {
        managed_network_refusal(runtime, "/udp-bind", b"OperationNotSupported");
    }

    #[tokio::test]
    async fn a_refused_managed_connection_preserves_its_socket_error_after_restoring(runtime: GuestRuntime) {
        let peer = std::net::SocketAddr::from(([127, 0, 0, 1], 1));
        let policy = isoplane_harness::Network::with_resolver(Box::new(NoDns))
            .with_destinations(Arc::new(ExactPeer(peer)))
            .with_connections(Arc::new(RefusedConnections));
        let owner = InstanceId::alloc();
        let reactor = Reactor::with_network(owner, policy);
        let context = &reactor.context;
        let futures = &reactor.futures;
        context.begin_slice(1, 1);
        let mut guest = ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
        let pending = guest.start(&format!("/connect/{}/{}", peer.ip(), peer.port()));
        assert_eq!(
            pending.completion,
            Completion::Parked,
            "ConnectAsync must return before its owned reactor runs"
        );
        let outcome = guest.wake(&reactor).await;
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
            reactor.runtime().services.egress.instance_connections(reactor.context.instance),
            0,
            "a refused connection must release its reserved quota"
        );
    }

    #[tokio::test]
    async fn a_managed_read_cancels_after_restoration_and_releases_its_connection(runtime: GuestRuntime) {
        stream_failure(runtime, "cancel", b"cancelled").await;
    }

    #[tokio::test]
    async fn a_managed_read_receives_clean_eof_after_restoration(runtime: GuestRuntime) {
        stream_failure(runtime, "eof", b"eof").await;
    }

    #[tokio::test]
    async fn a_managed_read_receives_a_peer_reset_after_restoration(runtime: GuestRuntime) {
        stream_failure(runtime, "reset", b"stream-error").await;
    }
}

async fn stream_failure(runtime: GuestRuntime, mode: &str, expected: &[u8]) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the stream failure test must own its peer");
    let peer = listener
        .local_addr()
        .expect("the peer must have an endpoint");
    let policy = isoplane_harness::Network::with_resolver(Box::new(NoDns))
        .with_destinations(Arc::new(ExactPeer(peer)));
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(owner, policy);
    let context = &reactor.context;
    let futures = &reactor.futures;
    context.begin_slice(1, 1);
    let mut guest =
        ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
    let outcome = guest.start(&format!("/stream/{}/{}/{mode}", peer.ip(), peer.port()));
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "the managed stream must return for its connection"
    );
    let (mut server, _) = tokio::time::timeout(WAKE_DEADLINE, listener.accept())
        .await
        .expect("the managed connection must arrive before the deadline")
        .expect("the peer must accept");
    let pending = guest.wake(&reactor).await;
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
            let outcome = guest.wake(&reactor).await;
            assert_stream_failure(&reactor, owner, outcome, expected).await;
            return;
        }
        _ => panic!("unknown controlled stream failure: {mode}"),
    }
    let outcome = guest.wake(&reactor).await;
    if mode == "cancel" {
        tokio::time::resume();
    }
    assert_stream_failure(&reactor, owner, outcome, expected).await;
    let mut extra = [0];
    let count = tokio::time::timeout(WAKE_DEADLINE, server.read(&mut extra))
        .await
        .expect("managed cleanup must reach the peer before the deadline")
        .expect("the peer must observe cleanup");
    assert_eq!(
        count, 0,
        "managed cancellation or EOF must dispose its host-owned connection"
    );
}

async fn assert_stream_failure(
    reactor: &Reactor,
    owner: InstanceId,
    outcome: isoplane_engine::DispatchOutcome,
    expected: &[u8],
) {
    isoplane_harness::LogCapture::global();
    let context = &reactor.context;
    let futures = &reactor.futures;
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
    tokio::time::timeout(WAKE_DEADLINE, async {
        while reactor
            .runtime()
            .services
            .egress
            .instance_connections(reactor.context.instance)
            != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("aborted stream work must release its connection before the deadline");
    assert_eq!(
        reactor
            .runtime()
            .services
            .egress
            .instance_connections(reactor.context.instance),
        0,
        "a terminal stream operation must release its connection quota"
    );
}

in_mono_runtime! {
    #[tokio::test]
    async fn an_unsupported_managed_socket_option_receives_operation_not_supported(runtime: GuestRuntime) {
        managed_network_refusal(runtime, "/hop-limit", b"OperationNotSupported");
    }
}

struct FailingDns(isoplane_abi::native_io::DnsFailure);
#[async_trait::async_trait]
impl Resolver for FailingDns {
    async fn resolve(
        &self,
        host: &str,
        _: u16,
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        Err(isoplane_host::egress::DnsError::Lookup {
            host: host.to_owned(),
            kind: self.0,
            message: "controlled resolver failure".into(),
        })
    }
}

async fn managed_dns_error(
    runtime: GuestRuntime,
    resolver: Box<dyn Resolver>,
    expected: &[u8],
    denials: u64,
) {
    isoplane_harness::LogCapture::global();
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(owner, isoplane_harness::Network::with_resolver(resolver));
    let context = &reactor.context;
    let futures = &reactor.futures;
    context.begin_slice(1, 1);
    let mut guest =
        ReturnedGuest::managed_probe("isoplane_guest_dotnet_native_io", runtime, &reactor);
    let started = guest.start("/dns/error.test");
    assert_eq!(
        started.completion,
        Completion::Parked,
        "DNS errors must arrive through owned async work: {started:?}"
    );
    let outcome = guest.wake(&reactor).await;
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
        reactor
            .runtime()
            .metrics
            .egress_denials
            .load(std::sync::atomic::Ordering::Relaxed),
        denials,
        "only a destination policy refusal must increment denial accounting"
    );
    assert_eq!(
        reactor
            .runtime()
            .services
            .egress
            .instance_connections(reactor.context.instance),
        0,
        "a failed or denied DNS answer must acquire no outbound connection"
    );
    assert_eq!(
        futures.pending_total(owner),
        0,
        "a failed DNS lookup must release its pending host work"
    );
}

in_mono_runtime! {
    #[tokio::test]
    async fn managed_dns_reports_a_missing_name_without_counting_a_denial(runtime: GuestRuntime) {
        managed_dns_error(
            runtime,
            Box::new(FailingDns(isoplane_abi::native_io::DnsFailure::NotFound)),
            b"HostNotFound",
            0,
        )
        .await;
    }

    #[tokio::test]
    async fn managed_dns_reports_a_retryable_resolver_failure_without_counting_a_denial(runtime: GuestRuntime) {
        managed_dns_error(
            runtime,
            Box::new(FailingDns(isoplane_abi::native_io::DnsFailure::Temporary)),
            b"TryAgain",
            0,
        )
        .await;
    }

    #[tokio::test]
    async fn managed_dns_reports_a_resolver_timeout_without_counting_a_denial(runtime: GuestRuntime) {
        managed_dns_error(
            runtime,
            Box::new(FailingDns(isoplane_abi::native_io::DnsFailure::Timeout)),
            b"TimedOut",
            0,
        )
        .await;
    }

    #[tokio::test]
    async fn managed_dns_reports_a_permanent_resolver_failure_without_counting_a_denial(runtime: GuestRuntime) {
        managed_dns_error(
            runtime,
            Box::new(FailingDns(isoplane_abi::native_io::DnsFailure::Permanent)),
            b"NoRecovery",
            0,
        )
        .await;
    }

    #[tokio::test]
    async fn managed_dns_refuses_a_mixed_public_private_answer_and_counts_the_policy_denial(runtime: GuestRuntime) {
        managed_dns_error(runtime, Box::new(MixedDns), b"AccessDenied", 1).await;
    }
}

struct MixedDns;
#[async_trait::async_trait]
impl Resolver for MixedDns {
    async fn resolve(
        &self,
        _: &str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, isoplane_host::egress::DnsError> {
        Ok(vec![
            std::net::SocketAddr::from(([93, 184, 216, 34], port)),
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        ])
    }
}

#[tokio::test]
async fn a_native_tcp_policy_refusal_counts_once_without_starting_owned_work() {
    use isoplane_abi::native_io::{AddressFamily, Error, Request, Response};
    use isoplane_engine::HostBindings;
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(
        owner,
        isoplane_harness::Network::with_resolver(Box::new(NoDns)),
    );
    let mut bindings = CellBindings::new(reactor.context.clone());
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
    let runtime = reactor.runtime();
    assert_eq!(
        runtime
            .metrics
            .egress_denials
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a refused native connection must record one denial"
    );
    assert_eq!(
        runtime.services.egress.instance_connections(owner),
        0,
        "a denied native destination must acquire no connection quota"
    );
    assert_eq!(
        reactor.futures.pending_total(owner),
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
impl isoplane_host::egress::ConnectionFactory for PendingConnections {
    async fn connect(
        &self,
        _: &isoplane_host::egress::ConnectionOwner,
        _: std::net::SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        let _attempt = PendingConnectionAttempt(self.cancelled.clone());
        self.started.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn dropping_a_socket_cancels_its_pending_connection_provider_and_releases_quota() {
    use isoplane_abi::native_io::{AddressFamily, Request, ResourceKind, Response};
    use isoplane_engine::HostBindings;
    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let policy = isoplane_harness::Network::with_resolver(Box::new(NoDns)).with_connections(
        Arc::new(PendingConnections {
            started: started.clone(),
            cancelled: cancelled.clone(),
        }),
    );
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(owner, policy);
    reactor.context.begin_slice(1, 1);
    let mut bindings = CellBindings::new(reactor.context.clone());
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
    tokio::time::timeout(WAKE_DEADLINE, started.notified())
        .await
        .expect("the connection provider must start before the test deadline");
    assert_eq!(
        reactor.futures.pending_total(owner),
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
        reactor.futures.pending_total(owner),
        0,
        "socket disposal must remove its pending wake immediately"
    );
    tokio::time::timeout(WAKE_DEADLINE, cancelled.notified())
        .await
        .expect("socket disposal must cancel the provider before the test deadline");
    assert_eq!(
        reactor
            .runtime()
            .services
            .egress
            .instance_connections(reactor.context.instance),
        0,
        "cancelling a pending connect must release its connection permit"
    );
}

/// Announces each connection the provider starts and never completes it.
struct HeldConnections(tokio::sync::mpsc::UnboundedSender<()>);

#[async_trait::async_trait]
impl isoplane_host::egress::ConnectionFactory for HeldConnections {
    async fn connect(
        &self,
        _: &isoplane_host::egress::ConnectionOwner,
        _: std::net::SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        self.0.send(()).map_err(std::io::Error::other)?;
        std::future::pending().await
    }
}

#[tokio::test]
async fn connects_beyond_the_instances_pending_limit_wait_instead_of_failing() {
    use isoplane_abi::native_io::{AddressFamily, Request, ResourceKind, Response};
    use isoplane_engine::HostBindings;
    let pending_limit = isoplane_harness::production_connection_limits()
        .ceilings()
        .in_progress;
    let (starts, mut started) = tokio::sync::mpsc::unbounded_channel();
    let policy = isoplane_harness::Network::with_resolver(Box::new(NoDns))
        .with_connections(Arc::new(HeldConnections(starts)));
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(owner, policy);
    reactor.context.begin_slice(1, 1);
    let mut bindings = CellBindings::new(reactor.context.clone());
    let handle = |bindings: &mut CellBindings, request| match bindings
        .native_io(request)
        .expect("the owned native resource must allocate")
    {
        Response::Handle(handle) => handle,
        other => panic!("native allocation must return a handle: {other:?}"),
    };
    let network = handle(&mut bindings, Request::Network);
    let attempts = pending_limit + 2;
    let sockets: Vec<_> = (0..attempts)
        .map(|_| handle(&mut bindings, Request::CreateTcp(AddressFamily::V4)))
        .collect();
    for &socket in &sockets {
        bindings
            .native_io(Request::Connect {
                socket,
                network,
                destination: std::net::SocketAddr::from(([93, 184, 216, 34], 443)),
            })
            .expect("a connect beyond the pending limit must start, not fail");
    }
    for _ in 0..pending_limit {
        tokio::time::timeout(WAKE_DEADLINE, started.recv())
            .await
            .expect("the attempts within the limit must start before the test deadline")
            .expect("the provider announces each start");
    }
    assert_eq!(
        reactor.futures.pending_total(owner),
        attempts as usize,
        "every attempt, queued or in progress, is one pending wake"
    );
    assert_eq!(
        reactor
            .runtime()
            .services
            .egress
            .instance_connections(owner),
        pending_limit,
        "only attempts in progress hold open permits; queued ones hold none"
    );
    let drop_socket = |bindings: &mut CellBindings, handle| {
        bindings
            .native_io(Request::Drop {
                handle,
                kind: ResourceKind::Tcp,
            })
            .expect("dropping a connecting socket must succeed");
    };
    drop_socket(&mut bindings, sockets[0]);
    tokio::time::timeout(WAKE_DEADLINE, started.recv())
        .await
        .expect("a finished attempt must let the next queued one start")
        .expect("the provider announces the start");
    let queued = sockets[sockets.len() - 1];
    drop_socket(&mut bindings, queued);
    let open = attempts - 2;
    tokio::time::timeout(WAKE_DEADLINE, async {
        while reactor
            .runtime()
            .services
            .egress
            .instance_connections(reactor.context.instance)
            != open
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the dropped sockets must release their permits before the test deadline");
    assert_eq!(
        reactor.futures.pending_total(owner),
        open as usize,
        "a dropped queued attempt removes its wake"
    );
    assert!(
        started.try_recv().is_err(),
        "a cancelled queued attempt must never reach the provider"
    );
}

/// One instance's native sockets, driven through the host binding as a guest drives them.
struct NativeSockets {
    bindings: CellBindings,
    network: isoplane_abi::native_io::ResourceHandle,
}

impl NativeSockets {
    fn new(reactor: &Reactor) -> Self {
        use isoplane_abi::native_io::{Request, Response};
        use isoplane_engine::HostBindings;
        reactor.context.begin_slice(1, 1);
        let mut bindings = CellBindings::new(reactor.context.clone());
        let Ok(Response::Handle(network)) = bindings.native_io(Request::Network) else {
            panic!("the instance network must exist")
        };
        Self { bindings, network }
    }

    /// Connects a new socket to `destination`, returning the socket and how its connect settled.
    async fn connect(
        &mut self,
        destination: std::net::SocketAddr,
    ) -> (
        isoplane_abi::native_io::ResourceHandle,
        Result<isoplane_abi::native_io::Response, isoplane_abi::native_io::Error>,
    ) {
        use isoplane_abi::native_io::{AddressFamily, Error, Request, Response};
        use isoplane_engine::HostBindings;
        let Ok(Response::Handle(socket)) = self
            .bindings
            .native_io(Request::CreateTcp(AddressFamily::V4))
        else {
            panic!("socket creation must return its handle")
        };
        self.bindings
            .native_io(Request::Connect {
                socket,
                network: self.network,
                destination,
            })
            .expect("an allowed destination must start its connect");
        let settled = tokio::time::timeout(WAKE_DEADLINE, async {
            loop {
                match self.bindings.native_io(Request::FinishConnect(socket)) {
                    Err(Error::WouldBlock) => tokio::task::yield_now().await,
                    settled => break settled,
                }
            }
        })
        .await
        .expect("the connect must settle before the test deadline");
        (socket, settled)
    }

    fn drop_handle(
        &mut self,
        handle: isoplane_abi::native_io::ResourceHandle,
        kind: isoplane_abi::native_io::ResourceKind,
    ) {
        use isoplane_engine::HostBindings;
        self.bindings
            .native_io(isoplane_abi::native_io::Request::Drop { handle, kind })
            .expect("an owned handle must be disposable");
    }
}

#[tokio::test]
async fn an_instance_at_its_open_connection_limit_is_refused_and_another_instance_is_not() {
    use isoplane_abi::native_io::{Error, ResourceKind, Response};
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("the owned peer must bind");
    let peer = listener.local_addr().expect("the owned peer's endpoint");
    let limit = isoplane_harness::production_connection_limits()
        .ceilings()
        .open;
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(
        owner,
        isoplane_harness::Network::with_resolver(Box::new(NoDns))
            .with_destinations(Arc::new(ExactPeer(peer))),
    );
    let egress = reactor.runtime().services.egress.clone();
    let denials = || {
        reactor
            .runtime()
            .metrics
            .egress_denials
            .load(std::sync::atomic::Ordering::Relaxed)
    };
    let mut sockets = NativeSockets::new(&reactor);
    let mut open = Vec::new();
    for index in 0..limit {
        let (socket, settled) = sockets.connect(peer).await;
        let Ok(Response::Streams { input, output }) = settled else {
            panic!("connection {index} is within the instance's limit: {settled:?}")
        };
        open.push((socket, input, output));
    }
    assert_eq!(egress.instance_connections(owner), limit);

    let (refused, settled) = sockets.connect(peer).await;
    assert!(
        matches!(&settled, Err(Error::Denied(reason))
            if reason.contains(&owner.to_string()) && reason.contains(&format!("limit of {limit} open"))),
        "the connection beyond the limit is refused, naming the instance and the limit: {settled:?}"
    );
    assert_eq!(denials(), 1, "the refusal is counted as one egress denial");
    assert_eq!(
        egress.instance_connections(owner),
        limit,
        "a refusal takes no open permit"
    );
    assert_eq!(
        reactor.futures.pending_total(owner),
        0,
        "a refusal leaves no reactor work behind"
    );
    sockets.drop_handle(refused, ResourceKind::Tcp);

    let sibling = InstanceId::alloc();
    let sibling_reactor = reactor.sibling(sibling);
    let mut other = NativeSockets::new(&sibling_reactor);
    let (_, settled) = other.connect(peer).await;
    assert!(
        matches!(settled, Ok(Response::Streams { .. })),
        "another instance of the same application has its own limit: {settled:?}"
    );
    assert_eq!(egress.instance_connections(sibling), 1);

    let (socket, input, output) = open.pop().expect("the instance holds open connections");
    sockets.drop_handle(input, ResourceKind::Input);
    sockets.drop_handle(output, ResourceKind::Output);
    assert_eq!(
        egress.instance_connections(owner),
        limit,
        "the socket handle still holds the connection"
    );
    sockets.drop_handle(socket, ResourceKind::Tcp);
    assert_eq!(
        egress.instance_connections(owner),
        limit - 1,
        "closing the socket and its streams returns its permit"
    );
    let (_, settled) = sockets.connect(peer).await;
    assert!(
        matches!(settled, Ok(Response::Streams { .. })),
        "a returned permit admits the instance again: {settled:?}"
    );
    assert_eq!(denials(), 1);
}

struct RefusedConnections;

#[async_trait::async_trait]
impl isoplane_host::egress::ConnectionFactory for RefusedConnections {
    async fn connect(
        &self,
        _: &isoplane_host::egress::ConnectionOwner,
        _: std::net::SocketAddr,
    ) -> std::io::Result<tokio::net::TcpStream> {
        Err(std::io::ErrorKind::ConnectionRefused.into())
    }
}

#[tokio::test]
async fn a_failed_connection_retains_readable_and_configurable_socket_options() {
    use isoplane_abi::native_io::{AddressFamily, Error, Request, ResourceKind, Response};
    use isoplane_engine::HostBindings;
    let owner = InstanceId::alloc();
    let reactor = Reactor::with_network(
        owner,
        isoplane_harness::Network::with_resolver(Box::new(NoDns))
            .with_connections(Arc::new(RefusedConnections)),
    );
    reactor.context.begin_dispatch(1, 1);
    let mut bindings = CellBindings::new(reactor.context.clone());
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
    assert!(
        matches!(
            bindings.native_io(Request::NoDelay {
                socket,
                enabled: Some(true)
            }),
            Ok(Response::NoDelay(true))
        ),
        "pre-connect configuration must be accepted"
    );
    bindings
        .native_io(Request::Connect {
            socket,
            network,
            destination: std::net::SocketAddr::from(([93, 184, 216, 34], 443)),
        })
        .expect("the controlled connection attempt must start");
    let result = tokio::time::timeout(WAKE_DEADLINE, async {
        loop {
            let result = bindings.native_io(Request::FinishConnect(socket));
            if !matches!(result, Err(Error::WouldBlock)) {
                break result;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the controlled connection failure must complete");
    assert!(
        matches!(
            result,
            Err(Error::Connection {
                kind: std::io::ErrorKind::ConnectionRefused,
                ..
            })
        ),
        "the socket must actually enter its failed-connect state"
    );
    for (enabled, expected) in [
        (None, true),
        (Some(false), false),
        (None, false),
        (Some(true), true),
        (None, true),
    ] {
        assert!(
            matches!(bindings.native_io(Request::NoDelay { socket, enabled }), Ok(Response::NoDelay(actual)) if actual == expected),
            "failed-connect sockets must retain their configurable NoDelay value"
        );
    }
    assert!(
        matches!(
            bindings.native_io(Request::FinishConnect(socket)),
            Err(Error::Connection {
                kind: std::io::ErrorKind::ConnectionRefused,
                ..
            })
        ),
        "option changes must not erase the original connection failure"
    );
    bindings
        .native_io(Request::Drop {
            handle: socket,
            kind: ResourceKind::Tcp,
        })
        .expect("a failed socket must remain disposable");
    assert!(
        matches!(bindings.native_io(Request::NoDelay { socket, enabled: None }), Err(Error::InvalidHandle(handle)) if handle == socket),
        "a disposed socket must not retain configuration authority"
    );
}
