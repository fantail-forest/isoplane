//! Outbound HTTP through the runtime's `wasi:http` handler, delegated to the
//! host's outbound HTTP capability: a request parks on the host's future
//! response, bodies stream both ways through `wasi:io/streams`, TLS
//! terminates at the host with the server certificate verified, and every
//! refusal the host makes reaches the ordinary `HttpRequestException` with
//! the `wasi:http` error code that names it.

#[macro_use]
mod common;
#[path = "common/admitted.rs"]
mod admitted;
#[path = "common/http_peers.rs"]
mod http_peers;
#[path = "common/refusing.rs"]
mod refusing;
#[path = "common/resolvers.rs"]
mod resolvers;
#[path = "common/tls_chains.rs"]
mod tls_chains;
#[path = "common/tls_material.rs"]
mod tls_material;

use std::sync::Arc;

use admitted::released;
use common::{guest, GuestRuntime};
use http_peers::{http_peer, https_peer, reactor_for_http, Certificate};
use refusing::refusing_endpoint;
use resolvers::{reactor_for_dns, FailingDns, FixedDns, PendingDns};
use wisp_engine::Completion;
use wisp_harness::EmittedGuestLogs as _;
use wisp_harness::{Reactor, WAKE_DEADLINE};

const LOCALHOST: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
/// SHA-256 of the 300 000-byte pattern the stream route serves.
const STREAM_DIGEST: &str = "3C65EA93424A9C362FEC0E3A69EA36031E8A358441479DD665CC6110EABE7B08";
/// SHA-256 of the 200 000-byte pattern the guest posts.
const POST_DIGEST: &str = "E24BC62381F1224FBBB74688663F8F9743B9680B193EDD666835E97B06E730EB";

/// A reactor admitting `peers`, resolving every name to the loopback address.
fn reactor(owner: u64, peers: Vec<std::net::SocketAddr>) -> Reactor {
    reactor_for_http(owner, peers, Box::new(FixedDns(vec![LOCALHOST])))
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_get_parks_on_the_host_response_and_resumes_with_it(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(1, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/get/127.0.0.1/{}", peer.address.port()));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "HttpClient.GetAsync must park on the host's response future: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            2,
            "an outstanding request registers two wake sources: its response and the client's timeout timer"
        );
        let text = guest.finish(&reactor, 8).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "the response head and body must reach the client; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.runtime().services.egress.connections(),
            0,
            "disposing the response must release the connection permit"
        );
    }

    #[tokio::test]
    async fn a_streamed_response_body_arrives_across_several_wakes(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(2, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/stream/127.0.0.1/{}/300000", peer.address.port()));
        assert_eq!(outcome.completion, Completion::Parked, "the response head parks: {outcome:?}");
        let (text, wakes) = guest.finish_counting(&reactor, 64).await;
        assert!(
            text.starts_with("status=200 content-type=application/octet-stream length=300000 "),
            "every byte of a chunked body must reach the client: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            text.ends_with(&format!(" sha256={STREAM_DIGEST}")),
            "the body must arrive intact and in order: {text}"
        );
        assert!(
            wakes >= 2,
            "a body larger than one host chunk must park for readiness more than once ({wakes} wakes)"
        );
        assert_eq!(reactor.runtime().services.egress.connections(), 0, "the connection is released after EOF");
    }

    #[tokio::test]
    async fn a_posted_body_streams_to_the_peer_and_is_echoed_back(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(3, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/post/127.0.0.1/{}/200000", peer.address.port()), 64).await;
        assert_eq!(
            text,
            format!("status=200 content-type=application/octet-stream sent={POST_DIGEST} echoed={POST_DIGEST}"),
            "the request body must reach the peer intact through the outgoing body stream; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn request_headers_reach_the_peer_and_response_headers_reach_the_client(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(4, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/headers/127.0.0.1/{}", peer.address.port()), 8).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=ping x-answer=pong",
            "headers must cross in both directions; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_redirect_is_returned_to_the_client_unfollowed(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(5, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/redirect/127.0.0.1/{}", peer.address.port()), 8).await;
        assert_eq!(
            text, "status=302 content-type=text/plain location=http://elsewhere.test/moved body=moved",
            "the host follows no redirect; the client sees the redirect itself"
        );
        assert_eq!(peer.connections(), 1, "one request, one connection");
    }

    #[tokio::test]
    async fn two_requests_in_flight_complete_independently(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(6, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/two/127.0.0.1/{}", peer.address.port()), 16).await;
        assert_eq!(text, "hello from peer+world", "each request must resume its own continuation");
        assert_eq!(reactor.runtime().services.egress.connections(), 0, "both connections are released");
    }

    #[tokio::test]
    async fn a_refused_connection_is_reported_by_its_error_code(runtime: GuestRuntime) {
        let (_reservation, refused) = refusing_endpoint();
        let reactor = reactor(7, vec![refused]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/get/127.0.0.1/{}", refused.port()), 8).await;
        assert_eq!(
            text, "HttpRequestException: CONNECTION_REFUSED",
            "a refused connection must reach the client as the wasi:http error code; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(reactor.runtime().services.egress.connections(), 0, "a failed request holds no permit");
    }

    #[tokio::test]
    async fn a_destination_off_the_policy_is_denied_before_any_connection(runtime: GuestRuntime) {
        let peer = http_peer().await;
        // The reactor admits no peer at all.
        let reactor = reactor(8, vec![]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/get/127.0.0.1/{}", peer.address.port()), 8).await;
        assert_eq!(
            text, "HttpRequestException: HTTP_REQUEST_DENIED",
            "a policy refusal must reach the client as a denial; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(peer.connections(), 0, "a denied request must never reach the peer");
        assert_eq!(reactor.runtime().services.egress.connections(), 0, "a denied request holds no permit");
    }

    #[tokio::test]
    async fn an_unresolvable_name_is_a_dns_error(runtime: GuestRuntime) {
        let reactor = reactor_for_dns(9, Box::new(FailingDns(wisp_abi::native_io::DnsFailure::NotFound)));
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, "/http/get/nowhere.test/80", 8).await;
        assert_eq!(
            text, "HttpRequestException: DNS_ERROR",
            "a resolver failure must reach the client as the DNS error code; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn cancelling_a_request_during_its_lookup_drops_the_host_lookup(runtime: GuestRuntime) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let stopped = Arc::new(tokio::sync::Notify::new());
        let reactor = reactor_for_dns(
            14,
            Box::new(PendingDns {
                entered: entered.clone(),
                stopped: stopped.clone(),
            }),
        );
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/http/cancel/pending.test/80/50");
        assert_eq!(outcome.completion, Completion::Parked, "the request parks on its lookup: {outcome:?}");
        tokio::time::timeout(WAKE_DEADLINE, entered.notified())
            .await
            .expect("the lookup must reach the resolver");
        reactor.context.begin_slice(1 + 50 + 1, 1);
        let text = guest.finish(&reactor, 8).await;
        assert_eq!(text, "OperationCanceledException", "the cancellation token ends the request");
        tokio::time::timeout(WAKE_DEADLINE, stopped.notified())
            .await
            .expect("dropping the cancelled request must stop the host lookup");
    }

    #[tokio::test]
    async fn the_client_timeout_cancels_a_stalled_request_and_releases_its_connection(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(10, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/timeout/127.0.0.1/{}/200", peer.address.port()));
        assert_eq!(outcome.completion, Completion::Parked, "the stalled request parks: {outcome:?}");
        // The client's timeout is a host timer; the response never comes. The
        // guest's clock is the dispatch's, advanced past the deadline for the
        // wake that delivers the timer.
        reactor.context.begin_slice(1 + 200 + 1, 1);
        let text = guest.finish(&reactor, 8).await;
        assert_eq!(
            text, "TaskCanceledException",
            "HttpClient.Timeout must cancel the request through the host timer; logs: {:?}",
            reactor.context.emitted_logs()
        );
        // Cancelling the request drops the host's connection.
        released(&reactor).await;
    }

    #[tokio::test]
    async fn cancelling_a_request_drops_the_host_exchange(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(11, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/cancel/127.0.0.1/{}/50", peer.address.port()));
        assert_eq!(outcome.completion, Completion::Parked, "the request parks: {outcome:?}");
        reactor.context.begin_slice(1 + 50 + 1, 1);
        let text = guest.finish(&reactor, 8).await;
        assert_eq!(text, "OperationCanceledException", "a cancellation token must end the request");
        // A cancelled request holds no permit.
        released(&reactor).await;
    }

    #[tokio::test]
    async fn https_terminates_at_the_host_with_the_server_certificate_verified(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::Leaf).await;
        let reactor = reactor(12, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/leaf.acceptance.test/{}", peer.address.port()), 8).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "a TLS request must complete against a certificate the host trusts; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(reactor.runtime().services.egress.connections(), 0, "the TLS connection is released");
    }

    #[tokio::test]
    async fn https_through_an_intermediate_chain_validates_against_the_anchor(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::Chained).await;
        let reactor = reactor(14, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/chained.acceptance.test/{}", peer.address.port()), 8).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "the intermediate the peer presents must chain its leaf to the host's anchor; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    /// The host's TLS performs no revocation check and fetches nothing: a
    /// complete chain validates by its anchor whatever URLs it carries, and
    /// a missing intermediate is a certificate error (superseded with the
    /// rest of host-terminated TLS by iteration 6).
    #[tokio::test]
    async fn https_at_the_host_neither_fetches_intermediates_nor_checks_revocation(runtime: GuestRuntime) {
        let complete = https_peer(Certificate::RemoteChained).await;
        let alone = https_peer(Certificate::RemoteAlone).await;
        let reactor = reactor(15, vec![complete.address, alone.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/remote.acceptance.test/{}", complete.address.port()), 8).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "a complete chain validates by its anchor without a revocation check; logs: {:?}",
            reactor.context.emitted_logs()
        );
        let text = guest.answer(&reactor, &format!("/http/https/remote.acceptance.test/{}", alone.address.port()), 8).await;
        assert_eq!(
            text, "HttpRequestException: TLS_CERTIFICATE_ERROR",
            "an intermediate only reachable by AIA is not fetched by the host; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_certificate_the_host_does_not_trust_is_a_tls_certificate_error(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::Stranger).await;
        let reactor = reactor(13, vec![peer.address]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/leaf.acceptance.test/{}", peer.address.port()), 8).await;
        assert_eq!(
            text, "HttpRequestException: TLS_CERTIFICATE_ERROR",
            "an untrusted server certificate must fail the request at the host; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(reactor.runtime().services.egress.connections(), 0, "a failed handshake holds no permit");
    }
}
