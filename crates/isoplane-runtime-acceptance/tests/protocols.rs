//! Managed HTTP proxy/authentication and ClientWebSocket through real guest
//! sockets, with a Store destroyed and restored at every asynchronous wait.

use isoplane_harness::logging::GuestLogs;
#[macro_use]
mod common;
#[path = "common/admitted.rs"]
mod admitted;
#[path = "common/fixed_dns.rs"]
mod fixed_dns;
#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/protocol_peers.rs"]
mod protocol_peers;
#[path = "common/tls_material.rs"]
pub mod tls_material;

use common::{guest, GuestRuntime};
use isoplane_engine::Completion;
use isoplane_harness::Reactor;
use isoplane_host::InstanceId;
use protocol_peers::{connect_proxy, protocol_peer, ProtocolPeer};

async fn exchange(runtime: GuestRuntime, mode: &str) -> (String, ProtocolPeer) {
    let peer = protocol_peer(mode == "ws-secure").await;
    let resolver: Box<dyn isoplane_host::Resolver> = if mode == "ws-secure" {
        Box::new(fixed_dns::FixedDns(vec![peer.address.ip()]))
    } else {
        Box::new(no_dns::NoDns)
    };
    let reactor = Reactor::with_network(
        InstanceId::alloc(),
        admitted::admitting(vec![peer.address], resolver),
    );
    let answer = request(
        runtime,
        &reactor,
        &format!("/protocols/{mode}/{}", peer.address.port()),
    )
    .await;
    if mode == "ws-default" {
        assert_eq!(reactor.futures.pending_total(reactor.context.instance), 0, "closing the default WebSocket must dispose its keepalive timer without an end_dispatch cleanup");
    }
    (answer, peer)
}

async fn request(runtime: GuestRuntime, reactor: &Reactor, path: &str) -> String {
    reactor.context.begin_slice(1_893_456_000_000, 1);
    let mut guest = guest(reactor, runtime);
    let started = guest.start(path);
    assert_eq!(
        started.completion,
        Completion::Parked,
        "the managed protocol must start real asynchronous socket I/O; logs: {:?}",
        reactor.context.logs()
    );
    let answer = guest.finish_through_deltas(reactor, 64).await;
    admitted::released(reactor).await;
    answer
}

async fn tunnel_exchange(runtime: GuestRuntime, authenticate: bool) {
    let origin = protocol_peer(true).await;
    let proxy = connect_proxy(origin.address, authenticate).await;
    let reactor = Reactor::with_network(
        InstanceId::alloc(),
        admitted::admitting(vec![proxy.address], Box::new(no_dns::NoDns)),
    );
    let mode = if authenticate {
        "proxy-tls-auth"
    } else {
        "proxy-tls"
    };
    let answer = request(
        runtime,
        &reactor,
        &format!(
            "/protocols/{mode}/{}/{}",
            proxy.address.port(),
            origin.address.port()
        ),
    )
    .await;
    assert_eq!(
        answer, "authenticated",
        "HTTPS must validate the origin through the proxy tunnel and return its response"
    );
    let requests = proxy.requests();
    assert_eq!(
        requests.len(),
        if authenticate { 2 } else { 1 },
        "the proxy must receive exactly the CONNECT attempts its authentication policy requires"
    );
    assert!(
        !requests[0].proxy_authorization,
        "the first CONNECT must not disclose credentials before a challenge"
    );
    if authenticate {
        assert!(
            requests[1].proxy_authorization,
            "the challenged CONNECT must carry proxy credentials"
        );
    }
    assert!(
        requests.iter().all(|request| !request.authorization),
        "the proxy must not receive origin credentials"
    );
    let requests = origin.requests();
    assert_eq!(
        requests.len(),
        1,
        "the tunnel must deliver one decrypted request to the origin"
    );
    assert_eq!(
        requests[0].uri, "/through-tunnel",
        "the origin must receive its own path inside TLS"
    );
    assert!(
        !requests[0].authorization && !requests[0].proxy_authorization,
        "proxy credentials must not leak through the TLS tunnel to the origin"
    );
}

in_mono_runtime! {
    #[tokio::test]
    async fn an_explicit_proxy_receives_the_absolute_uri_without_origin_dns(runtime: GuestRuntime) {
        let (answer, peer) = exchange(runtime, "proxy").await;
        assert_eq!(answer, "http://example.invalid/proxy", "the configured proxy must receive the absolute target instead of the guest resolving the origin");
        let requests = peer.requests();
        assert_eq!(requests.len(), 1, "an unauthenticated proxy request must not be retried");
        assert_eq!(requests[0].uri, answer, "the observed proxy request must contain the complete target URI");
    }

    #[tokio::test]
    async fn an_http_basic_challenge_retries_with_only_origin_credentials(runtime: GuestRuntime) {
        let (answer, peer) = exchange(runtime, "auth").await;
        assert_eq!(answer, "authenticated", "the origin must accept credentials after its challenge");
        let requests = peer.requests();
        assert_eq!(requests.len(), 2, "Basic authentication must first receive the challenge, then retry");
        assert!(!requests[0].authorization && requests[1].authorization, "origin credentials must appear on the challenged retry");
        assert!(requests.iter().all(|request| !request.proxy_authorization), "origin authentication must not manufacture proxy credentials");
    }

    #[tokio::test]
    async fn a_proxy_basic_challenge_retries_without_origin_credentials(runtime: GuestRuntime) {
        let (answer, peer) = exchange(runtime, "proxy-auth").await;
        assert_eq!(answer, "authenticated", "the proxy must accept its credentials after a 407 challenge");
        let requests = peer.requests();
        assert_eq!(requests.len(), 2, "proxy authentication must first receive the challenge, then retry");
        assert!(!requests[0].proxy_authorization && requests[1].proxy_authorization, "proxy credentials must appear on the challenged retry");
        assert!(requests.iter().all(|request| !request.authorization), "proxy credentials must not become origin credentials");
    }

    #[tokio::test]
    async fn a_websocket_text_message_and_close_survive_each_hibernation_delta(runtime: GuestRuntime) {
        let (answer, peer) = exchange(runtime, "ws-text").await;
        assert_eq!(answer, "echo and close", "ClientWebSocket must send fragmented text and receive its complete echo across restored Stores");
        assert!(peer.observed_close(), "the guest must perform the WebSocket close handshake before completing");
    }

    #[tokio::test]
    async fn a_secure_websocket_uses_normal_tls_validation_across_hibernation(runtime: GuestRuntime) {
        let (answer, peer) = exchange(runtime, "ws-secure").await;
        assert_eq!(answer, "echo and close", "ClientWebSocket must validate the configured trust root and hostname before exchanging messages through TLS");
        assert!(peer.observed_close(), "the TLS connection must carry the WebSocket close handshake");
    }

    #[tokio::test]
    async fn a_websocket_with_default_options_exchanges_messages_and_disposes_its_keepalive(runtime: GuestRuntime) {
        let (answer, peer) = exchange(runtime, "ws-default").await;
        assert_eq!(answer, "echo and close", "default ClientWebSocket options must work without the application disabling its keepalive timer");
        assert!(peer.observed_close(), "the default client must complete its close handshake");
    }

    #[tokio::test]
    async fn a_websocket_binary_message_survives_each_hibernation_delta(runtime: GuestRuntime) {
        let (answer, peer) = exchange(runtime, "ws-binary").await;
        assert_eq!(answer, "echo and close", "ClientWebSocket must preserve binary message type and bytes across restored Stores");
        assert!(peer.observed_close(), "the binary exchange must end with a close handshake");
    }

    #[tokio::test]
    async fn a_websocket_client_refuses_a_non_upgrade_response(runtime: GuestRuntime) {
        let (answer, _) = exchange(runtime, "ws-bad").await;
        assert_eq!(answer, "refused non-upgrade", "an ordinary HTTP response must not become a WebSocket connection");
    }

    #[tokio::test]
    async fn a_websocket_receive_observes_cancellation_and_releases_its_socket(runtime: GuestRuntime) {
        let (answer, _) = exchange(runtime, "ws-cancel").await;
        assert_eq!(answer, "receive cancelled", "an outstanding ClientWebSocket receive must finish through its cancellation token");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn an_https_connect_proxy_tunnels_tls_without_origin_dns(runtime: GuestRuntime) {
        tunnel_exchange(runtime, false).await;
    }

    #[tokio::test]
    async fn an_authenticated_https_connect_proxy_keeps_credentials_outside_the_tunnel(runtime: GuestRuntime) {
        tunnel_exchange(runtime, true).await;
    }
}
