//! Outbound HTTP through the runtime's own `SocketsHttpHandler` over the
//! delegated sockets: connections and reads park on socket readiness and
//! survive hibernation deltas mid-request, bodies stream both ways in guest
//! memory, redirects, decompression and pooling are the client's own, TLS
//! is the guest's `SslStream` with a root the application supplies, HTTP/2
//! is negotiated through that TLS's ALPN or spoken h2c with prior
//! knowledge while HTTP/3 stays off, and every failure reaches the ordinary
//! `HttpRequestException` with the socket error that caused it.

#[macro_use]
mod common;
#[path = "common/admitted.rs"]
mod admitted;
#[path = "common/fixed_dns.rs"]
mod fixed_dns;
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
use fixed_dns::FixedDns;
use http_peers::{
    goaway_peer, h2_https_peer, http_peer, http_peer_on, http_peer_on_listener, https_peer,
    https_peer_on, midstream_peer, pattern, reactor_for_http, stale_peer, BodyProgress,
    Certificate, MidstreamPeer,
};
use isoplane_engine::Completion;
use isoplane_harness::EmittedGuestLogs as _;
use isoplane_harness::{Reactor, ReturnedGuest, WAKE_DEADLINE};
use refusing::refusing_endpoint;
use resolvers::{reactor_for_dns, FailingDns, PendingDns};
use sha2::{Digest as _, Sha256};

const LOCALHOST: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
/// SHA-256 of the 300 000-byte pattern the stream route serves.
const STREAM_DIGEST: &str = "3C65EA93424A9C362FEC0E3A69EA36031E8A358441479DD665CC6110EABE7B08";
/// SHA-256 of the 200 000-byte pattern the guest posts.
const POST_DIGEST: &str = "E24BC62381F1224FBBB74688663F8F9743B9680B193EDD666835E97B06E730EB";
/// A wall clock inside the test material's validity, for the TLS cases.
const CLOCK_2030_MS: u64 = 1_893_456_000_000;

/// A reactor admitting `peers`, resolving every name to the loopback
/// address, its clock inside the material's validity.
fn reactor(owner: u64, peers: Vec<std::net::SocketAddr>) -> Reactor {
    let reactor = reactor_for_http(owner, peers, Box::new(FixedDns(vec![LOCALHOST])));
    reactor.context.begin_slice(CLOCK_2030_MS, 1);
    reactor
}

async fn reach_stalled_response(
    guest: &mut isoplane_harness::ReturnedGuest,
    reactor: &Reactor,
    peer: &http_peers::HttpPeer,
) {
    tokio::time::timeout(WAKE_DEADLINE, async {
        loop {
            tokio::select! {
                () = peer.stalled() => break,
                outcome = guest.wake(reactor) => assert_eq!(
                    outcome.completion, Completion::Parked,
                    "the request must reach the peer before cancellation: {outcome:?}"
                ),
            }
        }
    })
    .await
    .expect("the peer must receive the request before advancing the cancellation clock");
}

/// A prefix smaller than either body, used as an explicit guest/peer checkpoint.
const BODY_PREFIX_BYTES: usize = 8192;

async fn restore_at_body_checkpoint(
    guest: &mut ReturnedGuest,
    reactor: &Reactor,
    peer: &mut MidstreamPeer,
) -> BodyProgress {
    let progress = tokio::time::timeout(WAKE_DEADLINE, async {
        loop {
            tokio::select! {
                progress = peer.progress() => break progress,
                outcome = guest.wake(reactor) => assert_eq!(
                    outcome.completion, Completion::Parked,
                    "the guest must remain partial until its body gate opens: {outcome:?}"
                ),
            }
        }
    })
    .await
    .expect("the guest and peer must establish body progress before the test deadline");
    let outcome = guest.poll_through_delta().await;
    assert_eq!(outcome.completion, Completion::Parked,
        "the partially transferred body must restore and park while its remainder is withheld: {outcome:?}");
    progress
}

async fn response_body_survives_delta(runtime: GuestRuntime, owner: u64, http2: bool) {
    isoplane_harness::LogCapture::global();
    let mut peer = midstream_peer(BODY_PREFIX_BYTES, 300_000).await;
    let reactor = reactor(owner, vec![peer.address]);
    let mut guest = guest(&reactor, runtime);
    let selection = if http2 { "h2/" } else { "" };
    let outcome = guest.start(&format!(
        "/http/{selection}stream-gated/127.0.0.1/{}/300000/{BODY_PREFIX_BYTES}",
        peer.address.port()
    ));
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "the request parks: {outcome:?}"
    );
    let progress = restore_at_body_checkpoint(&mut guest, &reactor, &mut peer).await;
    let BodyProgress::Download { prefix_sha256 } = progress else {
        panic!("the guest must report download consumption: {progress:?}");
    };
    assert_eq!(
        prefix_sha256,
        format!("{:X}", Sha256::digest(pattern(BODY_PREFIX_BYTES))),
        "the guest must hash the exact prefix before the selected park is captured"
    );
    peer.release();
    let text = guest.finish_through_deltas(&reactor, 64).await;
    let version = if http2 { " version=2.0" } else { "" };
    assert!(
        text.starts_with(&format!("status=200{version} content-type=application/octet-stream length=300000 "))
            && text.ends_with(&format!(" sha256={STREAM_DIGEST}")),
        "the partially consumed response and its digest must survive restoration before the remainder arrives: {text}; logs: {:?}",
        reactor.context.emitted_logs()
    );
}

async fn request_body_survives_delta(runtime: GuestRuntime, owner: u64, http2: bool) {
    isoplane_harness::LogCapture::global();
    let mut peer = midstream_peer(BODY_PREFIX_BYTES, 200_000).await;
    let reactor = reactor(owner, vec![peer.address]);
    let mut guest = guest(&reactor, runtime);
    let selection = if http2 { "h2/" } else { "" };
    let outcome = guest.start(&format!(
        "/http/{selection}post-gated/127.0.0.1/{}/200000/{BODY_PREFIX_BYTES}",
        peer.address.port()
    ));
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "the request parks: {outcome:?}"
    );
    let progress = restore_at_body_checkpoint(&mut guest, &reactor, &mut peer).await;
    let BodyProgress::Upload { prefix } = progress else {
        panic!("the peer must report the partially produced upload: {progress:?}");
    };
    assert_eq!(
        prefix,
        pattern(BODY_PREFIX_BYTES),
        "the peer must receive the exact prefix while the guest producer withholds the remainder"
    );
    peer.release();
    let text = guest.finish_through_deltas(&reactor, 64).await;
    let version = if http2 { " version=2.0" } else { "" };
    assert_eq!(text,
        format!("status=200{version} content-type=application/octet-stream sent={POST_DIGEST} echoed={POST_DIGEST}"),
        "the partly produced upload must survive restoration before the producer sends the remainder; logs: {:?}",
        reactor.context.emitted_logs()
    );
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_get_parks_on_the_socket_and_resumes_with_the_response(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(1, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/get/127.0.0.1/{}", peer.address.port()));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "HttpClient.GetAsync must park on the connection instead of blocking: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        let text = guest.finish(&reactor, 16).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "the response head and body must reach the client; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(peer.connections(), 1, "one request, one IPv4 connection to the loopback peer");
    }

    #[tokio::test]
    async fn a_streamed_response_body_arrives_across_several_wakes(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(2, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/stream/127.0.0.1/{}/300000", peer.address.port()));
        assert_eq!(outcome.completion, Completion::Parked, "the request parks: {outcome:?}");
        let (text, wakes) = guest.finish_counting(&reactor, 64).await;
        assert!(
            text.starts_with("status=200 content-type=application/octet-stream length=300000 "),
            "every byte of a chunked body must reach the client: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(text.ends_with(&format!(" sha256={STREAM_DIGEST}")), "the body must arrive intact and in order: {text}");
        assert!(wakes >= 2, "a body larger than one socket read must park for readiness more than once ({wakes} wakes)");
    }

    #[tokio::test]
    async fn a_response_body_mid_stream_survives_a_hibernation_delta(runtime: GuestRuntime) {
        response_body_survives_delta(runtime, 3, false).await;
    }

    #[tokio::test]
    async fn a_posted_body_streams_to_the_peer_and_is_echoed_back(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(4, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/post/127.0.0.1/{}/200000", peer.address.port()), 64).await;
        assert_eq!(
            text,
            format!("status=200 content-type=application/octet-stream sent={POST_DIGEST} echoed={POST_DIGEST}"),
            "the request body must reach the peer intact; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_request_body_mid_stream_survives_a_hibernation_delta(runtime: GuestRuntime) {
        request_body_survives_delta(runtime, 5, false).await;
    }

    #[tokio::test]
    async fn request_headers_reach_the_peer_and_response_headers_reach_the_client(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(6, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/headers/127.0.0.1/{}", peer.address.port()), 16).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=ping x-answer=pong",
            "headers must cross in both directions; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_redirect_is_followed_by_default_and_returned_when_following_is_off(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(7, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/follow/127.0.0.1/{}", peer.address.port()), 16).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "a same-authority redirect is followed by the client; logs: {:?}",
            reactor.context.emitted_logs()
        );
        let text = guest.answer(&reactor, &format!("/http/redirect/127.0.0.1/{}", peer.address.port()), 16).await;
        assert_eq!(
            text, "status=302 content-type=text/plain location=http://elsewhere.test/moved body=moved",
            "with AllowAutoRedirect off the client sees the redirect itself"
        );
    }

    #[tokio::test]
    async fn a_redirect_chain_is_followed_up_to_the_limit_and_the_last_hop_is_returned_beyond_it(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(8, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/hops/127.0.0.1/{}/3", peer.address.port()), 32).await;
        assert_eq!(text, "status=200 content-type=text/plain body=landed", "three hops within a limit of three land; logs: {:?}", reactor.context.emitted_logs());
        let text = guest.answer(&reactor, &format!("/http/hops/127.0.0.1/{}/5", peer.address.port()), 32).await;
        assert_eq!(
            text, "status=302 content-type=text/plain location=/hops/1 body=moved",
            "beyond the limit the client stops and returns the last redirect unfollowed"
        );
    }

    #[tokio::test]
    async fn a_307_replays_the_post_with_its_body(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(9, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/replay/127.0.0.1/{}/50000", peer.address.port()), 32).await;
        assert_eq!(
            text, "status=200 content-type=application/octet-stream replayed=same",
            "a temporary redirect must resend the method and the whole body; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn gzip_deflate_and_brotli_responses_are_decompressed_by_the_client(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(10, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        for encoding in ["gzip", "deflate", "br"] {
            let text = guest.answer(&reactor, &format!("/http/{encoding}/127.0.0.1/{}/100000", peer.address.port()), 32).await;
            assert_eq!(
                text, "status=200 content-type=application/octet-stream content-encoding= body=decoded",
                "a {encoding} body must reach the application decoded, with the encoding header consumed; logs: {:?}",
                reactor.context.emitted_logs()
            );
        }
        let text = guest.answer(&reactor, &format!("/http/accept-encoding/127.0.0.1/{}", peer.address.port()), 16).await;
        assert_eq!(text, "gzip, deflate, br", "the client advertises the encodings it decodes");
    }

    #[tokio::test]
    async fn sequential_requests_on_one_client_reuse_one_pooled_connection(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(11, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/reuse/127.0.0.1/{}", peer.address.port()), 32).await;
        assert_eq!(text, "hello from peer|hello from peer|hello from peer", "every request completes; logs: {:?}", reactor.context.emitted_logs());
        assert_eq!(peer.connections(), 1, "three sequential requests must share one keep-alive connection");
    }

    #[tokio::test]
    async fn a_stale_pooled_connection_is_replaced_not_failed(runtime: GuestRuntime) {
        let peer = stale_peer().await;
        let reactor = reactor(12, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/stale/127.0.0.1/{}", peer.address.port()), 32).await;
        assert_eq!(
            text, "served once|served once",
            "a connection the peer closed while idle must be replaced by the pool, not surfaced as a failure; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(peer.connections(), 2, "the second request opens a fresh connection");
    }

    #[tokio::test]
    async fn two_requests_in_flight_complete_independently(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(13, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/two/127.0.0.1/{}", peer.address.port()), 32).await;
        assert_eq!(text, "hello from peer+world", "each request must resume its own continuation");
        assert_eq!(peer.connections(), 2, "two concurrent requests under the default policy stay on HTTP/1.1 in plaintext and take two connections");
    }

    #[tokio::test]
    async fn a_refused_connection_is_a_request_exception_over_the_socket_error(runtime: GuestRuntime) {
        let (_reservation, refused) = refusing_endpoint();
        let reactor = reactor(14, vec![refused]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/get/127.0.0.1/{}", refused.port()), 16).await;
        assert!(
            text.starts_with("HttpRequestException(ConnectionError): ") && text.ends_with("<- SocketException"),
            "a refused connection must reach the client as HttpRequestException over the socket error: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(reactor.runtime().services.egress.instance_connections(reactor.context.instance), 0, "a failed request holds no permit");
    }

    #[tokio::test]
    async fn a_destination_off_the_policy_is_denied_before_any_connection(runtime: GuestRuntime) {
        let peer = http_peer().await;
        // The reactor admits no peer at all.
        let reactor = reactor(15, vec![]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/get/127.0.0.1/{}", peer.address.port()), 16).await;
        assert!(
            text.starts_with("HttpRequestException(ConnectionError): ") && text.ends_with("<- SocketException"),
            "a policy refusal must reach the client as HttpRequestException over the socket error: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(peer.connections(), 0, "a denied request must never reach the peer");
        assert_eq!(reactor.runtime().services.egress.instance_connections(reactor.context.instance), 0, "a denied request holds no permit");
    }

    #[tokio::test]
    async fn an_unresolvable_name_is_a_request_exception_over_the_dns_error(runtime: GuestRuntime) {
        let reactor = reactor_for_dns(16, Box::new(FailingDns(isoplane_abi::native_io::DnsFailure::NotFound)));
        reactor.context.begin_slice(CLOCK_2030_MS, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, "/http/get/nowhere.test/80", 16).await;
        assert!(
            text.starts_with("HttpRequestException(NameResolutionError): ") && text.ends_with("<- SocketException"),
            "a resolver failure must reach the client as HttpRequestException over the DNS error: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn cancelling_a_request_during_its_lookup_drops_the_host_lookup(runtime: GuestRuntime) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let stopped = Arc::new(tokio::sync::Notify::new());
        let reactor = reactor_for_dns(
            17,
            Box::new(PendingDns {
                entered: entered.clone(),
                stopped: stopped.clone(),
            }),
        );
        reactor.context.begin_slice(CLOCK_2030_MS, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start("/http/cancel/pending.test/80/50");
        assert_eq!(outcome.completion, Completion::Parked, "the request parks on its lookup: {outcome:?}");
        tokio::time::timeout(WAKE_DEADLINE, entered.notified())
            .await
            .expect("the lookup must reach the resolver");
        reactor.context.begin_slice(CLOCK_2030_MS + 50 + 1, 1);
        let text = guest.finish(&reactor, 8).await;
        assert_eq!(text, "OperationCanceledException", "the cancellation token ends the request");
        // The pool keeps its connection attempt for the next request, as
        // .NET's does; the lookup ends with the dispatch, as every wake
        // source a request leaves behind does.
        assert!(
            reactor.futures.pending_total(reactor.context.instance) >= 1,
            "the pool's connection attempt outlives the cancelled request"
        );
        reactor.futures.end_dispatch(reactor.context.instance, 1);
        tokio::time::timeout(WAKE_DEADLINE, stopped.notified())
            .await
            .expect("ending the dispatch must stop the host lookup the pool left behind");
    }

    #[tokio::test]
    async fn the_client_timeout_cancels_a_stalled_request_and_releases_its_connection(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(18, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/timeout/127.0.0.1/{}/200", peer.address.port()));
        assert_eq!(outcome.completion, Completion::Parked, "the stalled request parks: {outcome:?}");
        reach_stalled_response(&mut guest, &reactor, &peer).await;
        // The client's timeout is a host timer; the response never comes. The
        // guest's clock is the dispatch's, advanced past the deadline for the
        // wake that delivers the timer.
        reactor.context.begin_slice(CLOCK_2030_MS + 200 + 1, 1);
        let text = guest.finish(&reactor, 16).await;
        assert_eq!(
            text, "TaskCanceledException",
            "HttpClient.Timeout must cancel the request through the host timer; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    #[tokio::test]
    async fn cancelling_a_request_releases_its_connection(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(19, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/cancel/127.0.0.1/{}/50", peer.address.port()));
        assert_eq!(outcome.completion, Completion::Parked, "the request parks: {outcome:?}");
        reach_stalled_response(&mut guest, &reactor, &peer).await;
        reactor.context.begin_slice(CLOCK_2030_MS + 50 + 1, 1);
        let text = guest.finish(&reactor, 16).await;
        assert_eq!(
            text, "OperationCanceledException",
            "a cancellation token must end the request; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    #[tokio::test]
    async fn https_is_the_guests_tls_with_the_applications_root(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::Leaf).await;
        let reactor = reactor(20, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/leaf.acceptance.test/{}", peer.address.port()), 32).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "a TLS request must complete against the root the application supplied, with the host holding no anchor; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(reactor.runtime().services.egress.instance_connections(reactor.context.instance), 0, "the TLS connection is released with the client");
    }

    #[tokio::test]
    async fn https_through_an_intermediate_chain_validates_against_the_applications_root(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::Chained).await;
        let reactor = reactor(21, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/chained.acceptance.test/{}", peer.address.port()), 32).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "the intermediate the peer presents must chain its leaf to the application's root; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_certificate_the_application_does_not_trust_fails_the_request(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::Stranger).await;
        let reactor = reactor(22, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/leaf.acceptance.test/{}", peer.address.port()), 32).await;
        assert!(
            text.starts_with("HttpRequestException(SecureConnectionError): ") && text.ends_with("<- AuthenticationException"),
            "an untrusted server certificate must fail the request through the TLS authentication error: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    /// The issuer's URL names a port nothing listens on: the retrieval
    /// fails and the chain stays partial, as it does off the network.
    #[tokio::test]
    async fn a_missing_intermediate_fails_the_request_when_its_issuer_cannot_be_reached(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::RemoteAlone).await;
        let reactor = reactor(23, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/remote.acceptance.test/{}", peer.address.port()), 64).await;
        assert!(
            text.starts_with("HttpRequestException(SecureConnectionError): ") && text.ends_with("<- AuthenticationException"),
            "an issuer that cannot be fetched leaves the chain partial and the request failed: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    #[tokio::test]
    async fn a_complete_chain_with_revocation_urls_validates_under_no_check(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::RemoteChained).await;
        let reactor = reactor(24, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/remote.acceptance.test/{}", peer.address.port()), 32).await;
        assert_eq!(
            text, "status=200 content-type=text/plain body=hello from peer",
            "the complete chain validates whatever revocation URLs it carries; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn an_exact_http_2_request_over_tls_is_negotiated_through_alpn(runtime: GuestRuntime) {
        let peer = h2_https_peer(Certificate::Leaf).await;
        let reactor = reactor(25, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/h2/https/leaf.acceptance.test/{}", peer.address.port()), 32).await;
        assert_eq!(
            text, "status=200 version=2.0 content-type=text/plain body=hello from peer",
            "a request pinned to HTTP/2 over TLS must complete on HTTP/2; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            peer.negotiated_alpn(),
            vec![Some("h2".to_owned())],
            "the guest's TLS must offer h2 through ALPN and the peer select it"
        );
    }

    #[tokio::test]
    async fn an_exact_http_2_request_in_plaintext_speaks_h2c_with_prior_knowledge(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(26, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/h2/get/127.0.0.1/{}", peer.address.port()), 32).await;
        assert_eq!(
            text, "status=200 version=2.0 content-type=text/plain body=hello from peer",
            "a request pinned to HTTP/2 over plaintext must open the connection with the h2 preface; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(peer.connections(), 1, "prior knowledge needs no upgrade round trip");
    }

    #[tokio::test]
    async fn a_peer_offering_only_http_1_1_answers_a_version_or_higher_request_on_1_1(runtime: GuestRuntime) {
        let peer = https_peer(Certificate::Leaf).await;
        let reactor = reactor(27, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/higher/https/leaf.acceptance.test/{}", peer.address.port()), 32).await;
        assert_eq!(
            text, "status=200 version=1.1 content-type=text/plain body=hello from peer",
            "RequestVersionOrHigher must settle on HTTP/1.1 when the peer offers nothing higher; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            peer.negotiated_alpn(),
            vec![Some("http/1.1".to_owned())],
            "the client offered h2 and http/1.1 through ALPN and the peer took http/1.1"
        );
    }

    #[tokio::test]
    async fn an_http_2_response_body_arrives_across_several_wakes(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(28, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/http/h2/stream/127.0.0.1/{}/300000", peer.address.port()));
        assert_eq!(outcome.completion, Completion::Parked, "the request parks: {outcome:?}");
        let (text, wakes) = guest.finish_counting(&reactor, 64).await;
        assert!(
            text.starts_with("status=200 version=2.0 content-type=application/octet-stream length=300000 "),
            "every DATA frame of an HTTP/2 body must reach the client: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(text.ends_with(&format!(" sha256={STREAM_DIGEST}")), "the body must arrive intact and in order: {text}");
        assert!(wakes >= 2, "a body larger than one socket read must park for readiness more than once ({wakes} wakes)");
    }

    #[tokio::test]
    async fn an_http_2_response_body_mid_stream_survives_a_hibernation_delta(runtime: GuestRuntime) {
        response_body_survives_delta(runtime, 29, true).await;
    }

    #[tokio::test]
    async fn an_http_2_request_body_streams_to_the_peer_and_is_echoed_back(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(30, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/h2/post/127.0.0.1/{}/200000", peer.address.port()), 64).await;
        assert_eq!(
            text,
            format!("status=200 version=2.0 content-type=application/octet-stream sent={POST_DIGEST} echoed={POST_DIGEST}"),
            "a request body sent as DATA frames under flow control must reach the peer intact; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn an_http_2_request_body_mid_stream_survives_a_hibernation_delta(runtime: GuestRuntime) {
        request_body_survives_delta(runtime, 31, true).await;
    }

    #[tokio::test]
    async fn two_concurrent_http_2_requests_multiplex_on_one_connection(runtime: GuestRuntime) {
        let peer = http_peer().await;
        let reactor = reactor(32, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/h2/two/127.0.0.1/{}", peer.address.port()), 32).await;
        assert_eq!(text, "hello from peer+world", "each request must resume its own continuation; logs: {:?}", reactor.context.emitted_logs());
        assert_eq!(peer.connections(), 1, "two concurrent HTTP/2 requests are streams of one connection");
    }

    #[tokio::test]
    async fn a_goaway_from_the_peer_makes_the_next_request_open_a_new_connection(runtime: GuestRuntime) {
        let peer = goaway_peer().await;
        let reactor = reactor(33, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/h2/reuse/127.0.0.1/{}", peer.address.port()), 64).await;
        assert_eq!(
            text, "hello from peer|hello from peer|hello from peer",
            "every request completes although each connection is told to go away; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(peer.connections(), 3, "a connection that received GOAWAY carries no further request");
    }

    #[tokio::test]
    async fn an_exact_http_3_request_fails_before_connecting_because_http_3_is_off(runtime: GuestRuntime) {
        let peer = h2_https_peer(Certificate::Leaf).await;
        let reactor = reactor(34, vec![peer.address]);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/h3/https/leaf.acceptance.test/{}", peer.address.port()), 32).await;
        // The SDK builds guests with `UseSystemResourceKeys`, so the
        // message is the resource key and its arguments rather than the
        // sentence "Requesting HTTP version 3.0 with version policy
        // RequestVersionExact while unable to establish HTTP/3 connection."
        assert_eq!(
            text,
            "HttpRequestException(VersionNegotiationError): net_http_requested_version_cannot_establish, 3.0, RequestVersionExact, 3",
            "HTTP/3 is switched off in the SDK, so a request pinned to it fails in the client; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(peer.connections(), 0, "a request the client cannot satisfy never reaches the peer");
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn an_ipv6_only_dns_destination_is_reachable_by_the_default_http_client(runtime: GuestRuntime) {
        let ipv6 = std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
        let peer = http_peer_on(ipv6).await;
        let reactor = reactor_for_http(81, vec![peer.address], Box::new(FixedDns(vec![ipv6])));
        reactor.context.begin_slice(CLOCK_2030_MS, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/get/ipv6.acceptance.test/{}", peer.address.port()), 32).await;
        assert_eq!(text, "status=200 content-type=text/plain body=hello from peer", "a hostname with only an IPv6 address must connect through the default handler");
        assert_eq!(peer.connections(), 1, "the IPv6 peer must receive the connection");
    }

    #[tokio::test]
    async fn an_ipv6_literal_is_reachable_by_the_default_http_client(runtime: GuestRuntime) {
        let ipv6 = std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
        let peer = http_peer_on(ipv6).await;
        let reactor = reactor_for_http(82, vec![peer.address], Box::new(FixedDns(vec![])));
        reactor.context.begin_slice(CLOCK_2030_MS, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/get/[::1]/{}", peer.address.port()), 32).await;
        assert_eq!(text, "status=200 content-type=text/plain body=hello from peer", "an IPv6 literal must connect without DNS or dual mode");
    }

    #[tokio::test]
    async fn https_authenticates_an_ipv6_only_destination(runtime: GuestRuntime) {
        let ipv6 = std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
        let peer = https_peer_on(Certificate::Leaf, ipv6).await;
        let reactor = reactor_for_http(83, vec![peer.address], Box::new(FixedDns(vec![ipv6])));
        reactor.context.begin_slice(CLOCK_2030_MS, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/https/leaf.acceptance.test/{}", peer.address.port()), 32).await;
        assert_eq!(text, "status=200 content-type=text/plain body=hello from peer", "TLS over IPv6 must preserve hostname and chain validation");
    }

    #[tokio::test]
    async fn an_ipv4_connection_failure_falls_back_to_the_dns_ipv6_address(runtime: GuestRuntime) {
        let ipv6 = std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST);
        let (_reservation, unavailable, listener) = loop {
            let (reservation, unavailable) = refusing_endpoint();
            match tokio::net::TcpListener::bind((ipv6, unavailable.port())).await {
                Ok(listener) => break (reservation, unavailable, listener),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(error) => panic!("the IPv6 fallback peer must bind: {error}"),
            }
        };
        let peer = http_peer_on_listener(listener);
        let reactor = reactor_for_http(84, vec![unavailable, peer.address], Box::new(FixedDns(vec![LOCALHOST, ipv6])));
        reactor.context.begin_slice(CLOCK_2030_MS, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/http/get/mixed.acceptance.test/{}", peer.address.port()), 32).await;
        assert_eq!(text, "status=200 content-type=text/plain body=hello from peer", "a failed IPv4 attempt must not suppress the next IPv6 address");
        assert_eq!(peer.connections(), 1, "the successful address must own exactly one peer connection");
    }
}
