//! TLS over the delegated sockets with the guest's own OpenSSL: an
//! `SslStream` handshake and its reads park on socket readiness like any
//! other socket I/O and survive Store restorations, bytes flow both ways,
//! the server certificate is validated against a root the guest trusts
//! (name, validity, chain), and every failure the peer can produce reaches
//! the ordinary .NET exception.

#[macro_use]
mod common;
#[path = "common/admitted.rs"]
mod admitted;
#[path = "common/chain_material.rs"]
mod chain_material;
#[path = "common/fixed_dns.rs"]
mod fixed_dns;
#[path = "common/material_peer.rs"]
mod material_peer;
#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/tls_chains.rs"]
mod tls_chains;
#[path = "common/tls_material.rs"]
mod tls_material;
#[path = "common/tls_peers.rs"]
mod tls_peers;

use admitted::{admitting, released};
use common::{guest, GuestRuntime};
use fixed_dns::FixedDns;
use material_peer::{Crl, Issuers, MaterialPeer, Ocsp, RetrievalLeaf, Sentinel};
use no_dns::NoDns;
use tls_peers::{tls_peer, Behaviour, Certificate, PeerEvent, TlsPeer};
use tokio::sync::mpsc::UnboundedReceiver;
use wisp_engine::Completion;
use wisp_harness::EmittedGuestLogs as _;
use wisp_harness::{Reactor, WAKE_DEADLINE};
use wisp_host::InstanceId;

/// A wall clock inside the test material's validity: the guest checks the
/// peer's certificate against its own clock, and the harness's default
/// clock is 1970, before any certificate's notBefore.
const CLOCK_2030_MS: u64 = 1_893_456_000_000;
/// A wall clock after the material's hundred-year validity.
const CLOCK_2200_MS: u64 = 7_258_118_400_000;

/// A reactor admitting exactly `peers` and resolving nothing, its clock at
/// `clock_ms`: the guest connects by address and names the peer only for
/// TLS. The clock never runs backwards, so a test needing another one
/// chooses it here.
fn reactor_at(peers: Vec<std::net::SocketAddr>, clock_ms: u64) -> Reactor {
    let reactor = Reactor::new(InstanceId::alloc(), admitting(peers, Box::new(NoDns)));
    reactor.context.begin_slice(clock_ms, 1);
    reactor
}

/// [`reactor_at`] with the clock inside the material's validity.
fn reactor_for_peers(peers: Vec<std::net::SocketAddr>) -> Reactor {
    reactor_at(peers, CLOCK_2030_MS)
}

/// [`reactor_for_peers`] whose guest reaches the peers by name, every name
/// resolving to the loopback (`HttpClient` names its host).
fn reactor_for_named_peers(peers: Vec<std::net::SocketAddr>) -> Reactor {
    let resolver = Box::new(FixedDns(vec![std::net::IpAddr::V4(
        std::net::Ipv4Addr::LOCALHOST,
    )]));
    let reactor = Reactor::new(InstanceId::alloc(), admitting(peers, resolver));
    reactor.context.begin_slice(CLOCK_2030_MS, 1);
    reactor
}

/// The pattern the guest sends: `length` bytes, as hex.
fn pattern(length: usize) -> String {
    (0..length)
        .map(|index| format!("{:02X}", (index % 251) as u8))
        .collect()
}

/// The handshake route against `peer`, naming the host and the chain
/// policy variant the guest applies.
fn handshake_route(peer: std::net::SocketAddr, host: &str, variant: &str) -> String {
    format!(
        "/tls/handshake/{}/{}/{host}/{variant}",
        peer.ip(),
        peer.port()
    )
}

/// The next thing the peer observed, before the test deadline.
async fn observed(events: &mut UnboundedReceiver<PeerEvent>) -> PeerEvent {
    tokio::time::timeout(WAKE_DEADLINE, events.recv())
        .await
        .expect("the peer must report before the test deadline")
        .expect("the peer outlives the test")
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_handshake_parks_on_socket_readiness_and_authenticates_the_peer(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/tls/handshake/{}/{}", peer.ip(), peer.port()));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the handshake must park on the socket instead of blocking: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        let text = guest.finish(&reactor, 32).await;
        assert!(
            text.starts_with("protocol=Tls1")
                && text.contains(" authenticated=True encrypted=True "),
            "the handshake must complete with a negotiated protocol: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            text.ends_with(" peer=CN=leaf.acceptance.test"),
            "the peer's certificate must be the leaf the trusted root signed: {text}"
        );
    }

    #[tokio::test]
    async fn bytes_round_trip_through_the_tls_stream(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let payload = pattern(4096);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/echo/{}/{}/{payload}", peer.ip(), peer.port()),
                64,
            )
            .await;
        assert_eq!(
            text,
            payload,
            "every byte must cross the TLS stream and come back; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_tls_session_survives_a_store_restoration_delta_round_trip_at_every_park(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let payload = pattern(2048);
        let outcome = guest.start(&format!(
            "/tls/echo/{}/{}/{payload}",
            peer.ip(),
            peer.port()
        ));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the handshake parks: {outcome:?}"
        );
        let text = guest.finish(&reactor, 64).await;
        assert_eq!(
            text, payload,
            "the TLS state and the connection must survive every park being captured and restored; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn two_tls_connections_in_flight_keep_their_own_sessions(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let (one, other) = (pattern(64), "CAFEBABE".repeat(8));
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/two/{}/{}/{one}/{other}", peer.ip(), peer.port()),
                64,
            )
            .await;
        assert_eq!(
            text,
            format!("{one}+{other}"),
            "each session must decrypt its own bytes"
        );
    }

    #[tokio::test]
    async fn a_chain_through_an_intermediate_validates_against_the_root(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Chained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/handshake/{}/{}/chained.acceptance.test",
                    peer.ip(),
                    peer.port()
                ),
                32,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=chained.acceptance.test"),
            "the intermediate the peer presents must chain the leaf to the trusted root: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_complete_chain_needs_no_retrieval(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Good, Ocsp::Good).await;
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "default"),
                32,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=remote.acceptance.test"),
            "a chain the peer completes validates: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            material.requests().is_empty(),
            "nothing is fetched for a complete chain under NoCheck: {:?}",
            material.requests()
        );
    }

    #[tokio::test]
    async fn a_missing_intermediate_is_retrieved_from_its_aia_url(runtime: GuestRuntime) {
        let material =
            MaterialPeer::start(Issuers::Served, Crl::Unavailable, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&handshake_route(peer, "remote.acceptance.test", "default"));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the connect parks: {outcome:?}"
        );
        let text = guest.finish(&reactor, 64).await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=remote.acceptance.test"),
            "the issuer at the AIA URL must complete the chain: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            material.requests(),
            vec!["/intermediate.der"],
            "exactly the missing issuer is fetched"
        );
    }

    #[tokio::test]
    async fn two_missing_intermediates_are_retrieved_iteratively_deepest_last(runtime: GuestRuntime) {
        let material =
            MaterialPeer::start(Issuers::Served, Crl::Unavailable, Ocsp::Unavailable).await;
        let peer = TlsPeer::presenting(RetrievalLeaf::Deep.presented(), Behaviour::Echo)
            .start()
            .await
            .0;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "deep.acceptance.test", "default"),
                64,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=deep.acceptance.test"),
            "each issuer names the next; the chain completes iteratively: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            material.requests(),
            vec!["/intermediate2.der", "/intermediate.der"],
            "the leaf's issuer is fetched first, its issuer second"
        );
    }

    #[tokio::test]
    async fn a_missing_intermediate_stays_partial_with_downloads_disabled(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Good, Ocsp::Good).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "downloadsoff"),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("PartialChain"),
            "with downloads explicitly disabled the chain stays partial: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            material.requests().is_empty(),
            "downloads disabled means no request: {:?}",
            material.requests()
        );
    }

    #[tokio::test]
    async fn online_revocation_with_a_good_crl_validates(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Good, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "onlineleaf"),
                64,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=remote.acceptance.test"),
            "a CRL that lists nothing passes the online check: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            material
                .requests()
                .contains(&"/intermediate.crl".to_owned()),
            "the CRL is fetched: {:?}",
            material.requests()
        );
    }

    #[tokio::test]
    async fn online_revocation_with_a_listing_crl_reports_revoked(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Revoked, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "onlineleaf"),
                64,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("Revoked"),
            "a CRL listing the leaf must fail it as revoked: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn online_revocation_with_an_expired_crl_reports_the_status_unknown(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Expired, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "onlineleaf"),
                64,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ")
                && text.contains("RevocationStatusUnknown")
                && text.contains("OfflineRevocation"),
            "an expired CRL and no responder leave the status unknown, never good: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn online_revocation_with_a_good_ocsp_response_validates(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Unavailable, Ocsp::LeafGood).await;
        let peer = TlsPeer::presenting(RetrievalLeaf::OcspLeaf.presented(), Behaviour::Echo)
            .start()
            .await
            .0;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "ocsp.acceptance.test", "onlineleaf"),
                64,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=ocsp.acceptance.test"),
            "a good OCSP response passes the online check with no CRL: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            material.requests().contains(&"/ocsp".to_owned()),
            "the responder is queried: {:?}",
            material.requests()
        );
    }

    #[tokio::test]
    async fn online_revocation_with_a_revoked_ocsp_response_reports_revoked(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Unavailable, Ocsp::Revoked).await;
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "onlineleaf"),
                64,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("Revoked"),
            "a revoked OCSP answer must fail the leaf as revoked: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn online_revocation_with_an_expired_ocsp_response_reports_the_status_unknown(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Unavailable, Ocsp::Expired).await;
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "onlineleaf"),
                64,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ")
                && text.contains("RevocationStatusUnknown"),
            "an OCSP answer past its next update is stale, never good: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn online_revocation_with_an_invalid_ocsp_response_reports_the_status_unknown(runtime: GuestRuntime) {
        for (ocsp, what) in [
            (Ocsp::Malformed, "a body that is not a response"),
            (Ocsp::Stranger, "a response the stranger signed"),
        ] {
            let material = MaterialPeer::start(Issuers::Served, Crl::Unavailable, ocsp).await;
            let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
            let reactor = reactor_for_peers(vec![peer, material.address()]);
            let mut guest = guest(&reactor, runtime);
            let text = guest
                .answer(
                    &reactor,
                    &handshake_route(peer, "remote.acceptance.test", "onlineleaf"),
                    64,
                )
                .await;
            assert!(
                text.starts_with("AuthenticationException: ")
                    && text.contains("RevocationStatusUnknown"),
                "{what} leaves the status unknown, never good: {text}; logs: {:?}",
                reactor.context.emitted_logs()
            );
        }
    }

    #[tokio::test]
    async fn offline_revocation_uses_the_cached_list_without_retrieving(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Good, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "onlineleaf"),
                64,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1"),
            "the online check fetches and caches the list: {text}"
        );
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "offlineleaf"),
                64,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=remote.acceptance.test"),
            "the offline check uses the cached list: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            material
                .requests()
                .iter()
                .filter(|path| path.as_str() == "/intermediate.crl")
                .count(),
            1,
            "the list is fetched once and served from the in-memory cache after: {:?}",
            material.requests()
        );
    }

    #[tokio::test]
    async fn a_retrieval_is_bounded_by_the_url_retrieval_timeout(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Hold, Ocsp::Hold).await;
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&handshake_route(
            peer,
            "remote.acceptance.test",
            "onlineleafbrief",
        ));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the connect parks: {outcome:?}"
        );
        let mut outcome = guest.wake(&reactor).await;
        // The handshake takes a few wakes; the retrieval parks with its timer.
        while outcome.completion == Completion::Parked
            && reactor.futures.pending_total(reactor.context.instance) < 2
        {
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the retrieval parks: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        // A timer is due by the guest's clock, which only the test moves: each
        // wake from here (the list's timeout, then the responder's) sees the
        // clock past any timer armed at the previous one.
        let mut clock_ms = CLOCK_2030_MS;
        let mut wakes = 0;
        while outcome.completion == Completion::Parked {
            assert!(
                wakes < 32,
                "the bounded retrievals must finish within 32 wakes; logs: {:?}",
                reactor.context.emitted_logs()
            );
            clock_ms += 500 + 1;
            reactor.context.begin_slice(clock_ms, 1);
            outcome = guest.wake(&reactor).await;
            wakes += 1;
        }
        let text = guest.text_of_outcome(outcome);
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("RevocationStatusUnknown"),
            "a responder that never answers is bounded by the policy's timeout and leaves the status unknown: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    #[tokio::test]
    async fn a_retrieval_is_bounded_by_the_download_limit(runtime: GuestRuntime) {
        let material =
            MaterialPeer::start(Issuers::Oversized, Crl::Unavailable, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "remote.acceptance.test", "limit"),
                64,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("PartialChain"),
            "an issuer past the download limit is not taken and the chain stays partial: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_cancelled_authentication_cancels_a_pending_retrieval(runtime: GuestRuntime) {
        let material =
            MaterialPeer::start(Issuers::Hold, Crl::Unavailable, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!(
            "/tls/cancel-handshake/{}/{}/200",
            peer.ip(),
            peer.port()
        ));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the connect parks: {outcome:?}"
        );
        let mut outcome = guest.wake(&reactor).await;
        while outcome.completion == Completion::Parked && material.requests().is_empty() {
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the retrieval parks on the held request: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        reactor.context.begin_slice(CLOCK_2030_MS + 200 + 1, 1);
        let text = guest.finish(&reactor, 32).await;
        assert_eq!(
            text,
            "cancelled",
            "the token cancels the authentication while its retrieval is pending; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    #[tokio::test]
    async fn a_pending_retrieval_survives_a_store_restoration_at_every_park(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Good, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&handshake_route(
            peer,
            "remote.acceptance.test",
            "onlineleaf",
        ));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the connect parks: {outcome:?}"
        );
        let text = guest.finish(&reactor, 64).await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=remote.acceptance.test"),
            "the half-built chain and its pending retrievals survive every park being captured and restored: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            material.requests(),
            vec!["/intermediate.der", "/intermediate.crl"],
            "the issuer and then the list are fetched across the restores"
        );
    }

    #[tokio::test]
    async fn a_missing_intermediate_is_retrieved_through_aia_for_https(runtime: GuestRuntime) {
        let material =
            MaterialPeer::start(Issuers::Served, Crl::Unavailable, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::HttpHello).await;
        let reactor = reactor_for_named_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/http/https/remote.acceptance.test/{}", peer.port()),
                64,
            )
            .await;
        assert_eq!(
            text,
            "status=200 content-type=text/plain body=hello from peer",
            "HttpClient's TLS completes the chain through the issuer's AIA URL; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            material.requests(),
            vec!["/intermediate.der"],
            "the missing issuer is fetched over the same sockets"
        );
    }

    #[tokio::test]
    async fn the_synchronous_handshake_fails_rather_than_waiting(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/sync-auth/{}/{}", peer.ip(), peer.port()),
                8,
            )
            .await;
        assert!(
            text.starts_with("failed: PlatformNotSupportedException: "),
            "the runtime refuses the synchronous handshake outright (it cannot wait on the guest's only thread): {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_synchronous_read_with_nothing_to_read_fails_rather_than_waiting(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Quiet).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/sync-read/{}/{}", peer.ip(), peer.port()),
                16,
            )
            .await;
        assert!(
            text.starts_with("write failed: IOException: "),
            "synchronous I/O on an established session fails as I/O at the first write (the socket refuses to block): {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_direct_synchronous_chain_build_uses_local_material_and_fetches_nothing(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Good, Ocsp::Good).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/sync-chain/{}/{}/remote.acceptance.test",
                    peer.ip(),
                    peer.port()
                ),
                32,
            )
            .await;
        assert_eq!(
            text, "built=False elements=1 status=PartialChain",
            "the synchronous build reports the ordinary failure for an issuer it does not have; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            material.requests().is_empty(),
            "the synchronous build fetches nothing: {:?}",
            material.requests()
        );
    }

    #[tokio::test]
    async fn a_direct_synchronous_chain_build_finds_the_issuer_an_asynchronous_build_fetched(runtime: GuestRuntime) {
        let material = MaterialPeer::start(Issuers::Served, Crl::Good, Ocsp::Good).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer, material.address()]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/sync-chain-after-async/{}/{}/remote.acceptance.test",
                    peer.ip(),
                    peer.port()
                ),
                64,
            )
            .await;
        assert_eq!(
            text, "built=True elements=3 status=NoError",
            "the issuer the asynchronous authentication fetched is local material for the synchronous build; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            material.requests(),
            vec!["/intermediate.der"],
            "the issuer is fetched once, by the asynchronous build"
        );
    }

    #[tokio::test]
    async fn a_certificate_directed_request_obeys_the_destination_policy(runtime: GuestRuntime) {
        let peer = TlsPeer::presenting(RetrievalLeaf::Denied.presented(), Behaviour::Echo)
            .start()
            .await
            .0;
        // Something listens where the issuer's URL points, and must never hear
        // from the guest: the policy admits the TLS peer alone.
        let sentinel = Sentinel::start().await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &handshake_route(peer, "denied.acceptance.test", "default"),
                64,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("PartialChain"),
            "an issuer at a destination the policy refuses is not fetched and the chain stays partial: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor
                .runtime()
                .metrics
                .egress_denials
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the refusal is the policy's, counted once"
        );
        assert_eq!(
            sentinel.reached(),
            0,
            "the refused destination never hears from the guest"
        );
    }

    #[tokio::test]
    async fn online_revocation_with_an_unavailable_responder_is_not_a_pass(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/handshake/{}/{}/remote.acceptance.test/online",
                    peer.ip(),
                    peer.port()
                ),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ")
                && text.contains("RevocationStatusUnknown")
                && text.contains("OfflineRevocation"),
            "revocation information that cannot be fetched must fail validation, never pass it: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn offline_revocation_without_a_cached_list_is_not_a_pass(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/handshake/{}/{}/remote.acceptance.test/offline",
                    peer.ip(),
                    peer.port()
                ),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("RevocationStatusUnknown"),
            "an offline check with no cached list must report the status unknown: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_complete_chain_with_no_revocation_check_validates(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::RemoteChained, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/handshake/{}/{}/remote.acceptance.test",
                    peer.ip(),
                    peer.port()
                ),
                32,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=remote.acceptance.test"),
            "the complete chain validates with NoCheck regardless of the URLs it carries: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_certificate_the_guest_does_not_trust_fails_authentication(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Stranger, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/untrusted/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: "),
            "an untrusted certificate must fail the handshake as .NET does: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.runtime().services.egress.connections(),
            0,
            "a failed handshake releases its connection"
        );
    }

    /// This fixture serves no platform trust bundle and supplies no application roots.
    #[tokio::test]
    async fn default_trust_holds_no_roots_so_even_the_leaf_is_untrusted(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/untrusted/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("UntrustedRoot"),
            "without a platform bundle or application root the chain is untrusted: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_name_the_certificate_does_not_cover_fails_authentication(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/handshake/{}/{}/other.acceptance.test",
                    peer.ip(),
                    peer.port()
                ),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ")
                && text.contains("RemoteCertificateNameMismatch"),
            "a target host the leaf does not name must be a name mismatch: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_certificate_not_yet_valid_by_the_guest_clock_fails_authentication(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_at(vec![peer], 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/handshake/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("NotTimeValid"),
            "validity is checked against the guest's clock, 1970 here: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn an_expired_certificate_by_the_guest_clock_fails_authentication(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_at(vec![peer], CLOCK_2200_MS);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/handshake/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("NotTimeValid"),
            "a certificate past notAfter by the guest's clock is expired: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn the_validation_callback_sees_the_chain_errors_and_may_accept(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Stranger, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/callback/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert!(
            text.starts_with("accepted RemoteCertificateNameMismatch, RemoteCertificateChainErrors protocol=Tls1"),
            "the application's callback must see the chain and name errors and decide: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            text.ends_with(" peer=CN=Stranger Root"),
            "the accepted peer is the stranger: {text}"
        );
    }

    #[tokio::test]
    async fn a_client_certificate_the_peer_requires_is_presented_with_its_key(runtime: GuestRuntime) {
        let (peer, mut events) = TlsPeer::new(Certificate::Leaf, Behaviour::Echo)
            .requiring_client_certificate()
            .start()
            .await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let payload = pattern(32);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/client/{}/{}/{payload}", peer.ip(), peer.port()),
                64,
            )
            .await;
        assert_eq!(
            text,
            format!("mutual=True {payload}"),
            "the guest's certificate and private key must complete a mutually authenticated session; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            matches!(
                observed(&mut events).await,
                PeerEvent::Handshake {
                    client_certificate: Some(_),
                    ..
                }
            ),
            "the peer must have verified the guest's certificate against the root"
        );
    }

    #[tokio::test]
    async fn a_required_client_certificate_the_guest_lacks_fails_the_session(runtime: GuestRuntime) {
        let (peer, _events) = TlsPeer::new(Certificate::Leaf, Behaviour::Echo)
            .requiring_client_certificate()
            .start()
            .await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/client-none/{}/{}", peer.ip(), peer.port()),
                64,
            )
            .await;
        assert!(
            text.starts_with("read: IOException") || text.starts_with("handshake: "),
            "the peer's certificate_required alert must surface as an error, not as data: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.runtime().services.egress.connections(),
            0,
            "the refused session releases its connection"
        );
    }

    #[tokio::test]
    async fn alpn_negotiates_the_protocol_both_sides_offer(runtime: GuestRuntime) {
        let (peer, mut events) = TlsPeer::new(Certificate::Leaf, Behaviour::Echo)
            .with_alpn(&["wisp-acceptance", "http/1.1"])
            .start()
            .await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/alpn/{}/{}/wisp-acceptance", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert_eq!(
            text,
            "alpn=wisp-acceptance",
            "the negotiated protocol must be the one both offered; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            observed(&mut events).await,
            PeerEvent::Handshake {
                alpn: Some(b"wisp-acceptance".to_vec()),
                client_certificate: None
            },
            "the peer must agree on the protocol"
        );
    }

    #[tokio::test]
    async fn a_handshake_is_cancelled_by_its_token_while_parked(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Silent).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!(
            "/tls/cancel-handshake/{}/{}/200",
            peer.ip(),
            peer.port()
        ));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the connect parks: {outcome:?}"
        );
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the handshake parks on a peer that never answers: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        // The token's timer is a host timer; the guest's clock is the
        // dispatch's, advanced past the deadline for the wake that delivers it.
        reactor.context.begin_slice(CLOCK_2030_MS + 200 + 1, 1);
        let text = guest.finish(&reactor, 8).await;
        assert_eq!(
            text,
            "cancelled",
            "the cancellation must end the handshake as OperationCanceledException; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    #[tokio::test]
    async fn a_read_is_cancelled_by_its_token_while_parked(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!(
            "/tls/cancel-read/{}/{}/200",
            peer.ip(),
            peer.port()
        ));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the connect parks: {outcome:?}"
        );
        let mut outcome = guest.wake(&reactor).await;
        // The handshake takes a few wakes; the read parks with the timer.
        while outcome.completion == Completion::Parked
            && reactor.futures.pending_total(reactor.context.instance) < 2
        {
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "the read parks: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        reactor.context.begin_slice(CLOCK_2030_MS + 200 + 1, 1);
        let text = guest.finish(&reactor, 8).await;
        assert_eq!(
            text,
            "cancelled",
            "the cancellation must end the read as OperationCanceledException; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    #[tokio::test]
    async fn a_connection_dropped_during_the_handshake_is_an_io_failure(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::CloseDuringHandshake).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/handshake/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert!(
            text.starts_with("IOException: "),
            "a peer that vanishes mid-handshake must fail the handshake with an I/O error: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.runtime().services.egress.connections(),
            0,
            "the connection is released"
        );
    }

    #[tokio::test]
    async fn a_record_truncated_by_the_peer_is_an_io_failure_not_data(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::TruncatedRecord).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/eof/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert!(
            text.starts_with("IOException: "),
            "half a record followed by a drop must be an I/O error, never a short read or EOF: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_peer_closing_after_the_handshake_reads_as_end_of_stream(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Eof).await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/eof/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert_eq!(
            text,
            "eof",
            "a clean TLS close must read as end of stream; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn shutting_down_sends_close_notify_before_the_socket_goes(runtime: GuestRuntime) {
        let (peer, mut events) = TlsPeer::new(Certificate::Leaf, Behaviour::Echo)
            .start()
            .await;
        let reactor = reactor_for_peers(vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!("/tls/shutdown/{}/{}", peer.ip(), peer.port()),
                32,
            )
            .await;
        assert_eq!(
            text,
            "shut down",
            "the shutdown must complete; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert!(
            matches!(observed(&mut events).await, PeerEvent::Handshake { .. }),
            "the peer completes the handshake first"
        );
        assert_eq!(
            observed(&mut events).await,
            PeerEvent::Closed { clean: true },
            "the peer must see the close_notify, not a dropped connection"
        );
    }
}
