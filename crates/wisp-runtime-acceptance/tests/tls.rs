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
#[path = "common/trust_bundle.rs"]
mod trust_bundle;

use admitted::{admitting, released};
use common::{guest, GuestRuntime};
use fixed_dns::FixedDns;
use material_peer::{Crl, Issuers, MaterialPeer, Ocsp, RetrievalLeaf, Sentinel};
use no_dns::NoDns;
use tls_chains::parse_chain;
use tls_peers::{tls_peer, Behaviour, Certificate, PeerEvent, TlsPeer};
use tokio::sync::mpsc::UnboundedReceiver;
use trust_bundle::bundle;
use wisp_engine::Completion;
use wisp_engine::PlatformFiles;
use wisp_harness::EmittedGuestLogs as _;
use wisp_harness::{Reactor, ReturnedGuest, WAKE_DEADLINE};
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
    async fn default_trust_and_retrieval_together_complete_a_presented_leaf(runtime: GuestRuntime) {
        // The platform bundle holds the root, the peer presents its leaf
        // alone: the issuer is fetched and the chain validates with no
        // application trust configuration.
        let material = MaterialPeer::start(Issuers::Served, Crl::Unavailable, Ocsp::Unavailable).await;
        let peer = tls_peer(Certificate::RemoteAlone, Behaviour::Echo).await;
        let reactor = Reactor::with_platform_files(
            InstanceId::alloc(),
            admitting(vec![peer, material.address()], Box::new(NoDns)),
            bundle(&[tls_material::ROOT_CERTIFICATE]),
        );
        reactor.context.begin_slice(CLOCK_2030_MS, 1);
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, &format!("/tls/system/{}/{}/remote.acceptance.test", peer.ip(), peer.port()), 64).await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=remote.acceptance.test"),
            "a leaf presented alone completes through AIA and validates against the platform bundle: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(material.requests(), vec!["/intermediate.der"], "the issuer was fetched once");
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
    async fn with_no_bundle_served_default_trust_holds_no_roots(runtime: GuestRuntime) {
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

// The platform's default trust: the root bundle the host serves at
// `/etc/ssl/certs/ca-certificates.crt`, read by the guest's own TLS stack
// with no application trust configuration, independent of anything on the
// host, and replaced by explicit custom trust. Rotation is what a guest sees
// when the served file changes: its store provider rechecks the file when
// it is next used and at least five seconds of guest time have passed since
// the last check, so these tests move the clock and say exactly when the
// old and the new roots are observed.

/// Past the store provider's five-second recheck interval.
const RECHECK_MS: u64 = 6_000;

fn reactor_serving(peers: Vec<std::net::SocketAddr>, files: PlatformFiles) -> Reactor {
    let reactor = Reactor::with_platform_files(
        InstanceId::alloc(),
        admitting(peers, Box::new(NoDns)),
        files,
    );
    reactor.context.begin_slice(CLOCK_2030_MS, 1);
    reactor
}

fn system_route(peer: std::net::SocketAddr, host: &str) -> String {
    format!("/tls/system/{}/{}/{host}", peer.ip(), peer.port())
}

fn root_thumbprint(pem: &str) -> String {
    let (chain, _) = parse_chain(pem, tls_material::LEAF_KEY);
    let digest = <sha1::Sha1 as sha1::Digest>::digest(chain[0].as_ref());
    digest.iter().map(|byte| format!("{byte:02X}")).collect()
}

/// A guest that validated once under `first`, so its store provider holds
/// those roots, then sees `second` served instead.
async fn validated_then_rotated(
    runtime: GuestRuntime,
    peer: std::net::SocketAddr,
    first: &[&str],
    second: &[&str],
) -> (Reactor, ReturnedGuest) {
    let reactor = reactor_serving(vec![peer], bundle(first));
    let mut guest = guest(&reactor, runtime);
    let text = guest
        .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
        .await;
    assert!(
        text.starts_with("protocol=Tls1"),
        "the first validation trusts the bundle's root: {text}"
    );
    guest.serve_platform_files(bundle(second));
    (reactor, guest)
}

in_both_runtimes! {
    #[tokio::test]
    async fn default_trust_validates_a_complete_chain_from_the_platform_bundle(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Chained, Behaviour::Echo).await;
        let reactor = reactor_serving(vec![peer], bundle(&[tls_material::ROOT_CERTIFICATE]));
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(&reactor, &system_route(peer, "chained.acceptance.test"), 32)
            .await;
        assert!(
            text.starts_with("protocol=Tls1") && text.ends_with(" peer=CN=chained.acceptance.test"),
            "a complete chain validates against the platform bundle with no application trust configuration: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn default_trust_rejects_a_root_outside_the_bundle(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Stranger, Behaviour::Echo).await;
        let reactor = reactor_serving(vec![peer], bundle(&[tls_material::ROOT_CERTIFICATE]));
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &system_route(peer, "stranger.acceptance.test"),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("RemoteCertificateChainErrors"),
            "a root the bundle does not hold is untrusted (the stranger's name is not the peer's either, so the chain error comes with the name mismatch): {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        released(&reactor).await;
    }

    #[tokio::test]
    async fn default_trust_does_not_excuse_a_wrong_host_name(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_serving(vec![peer], bundle(&[tls_material::ROOT_CERTIFICATE]));
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(&reactor, &system_route(peer, "other.acceptance.test"), 32)
            .await;
        assert!(
            text.starts_with("AuthenticationException: ")
                && text.contains("RemoteCertificateNameMismatch"),
            "the bundle trusts the root, the name check still fails: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn explicit_custom_trust_replaces_the_bundle_rather_than_adding_to_it(runtime: GuestRuntime) {
        // Custom trust of the material's root, under a bundle holding only the
        // stranger: the leaf validates (custom trust decides) and the stranger
        // does not (the bundle is not consulted).
        let leaf = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let stranger = tls_peer(Certificate::Stranger, Behaviour::Echo).await;
        let reactor = reactor_serving(
            vec![leaf, stranger],
            bundle(&[tls_material::STRANGER_CERTIFICATE]),
        );
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/handshake/{}/{}/leaf.acceptance.test/default",
                    leaf.ip(),
                    leaf.port()
                ),
                32,
            )
            .await;
        assert!(
            text.starts_with("protocol=Tls1"),
            "custom trust validates its own root: {text}"
        );
        let text = guest
            .answer(
                &reactor,
                &format!(
                    "/tls/handshake/{}/{}/stranger.acceptance.test/default",
                    stranger.ip(),
                    stranger.port()
                ),
                32,
            )
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("RemoteCertificateChainErrors"),
            "custom trust replaces the bundle: a bundle root is untrusted under it: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn trust_is_the_served_bundle_and_nothing_ambient(runtime: GuestRuntime) {
        // The host running these tests has its own CAs; the guest sees
        // exactly the served file: nothing when it is empty.
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_serving(vec![peer], bundle(&[]));
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("UntrustedRoot"),
            "an empty bundle trusts nothing, whatever the host machine trusts: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        let text = guest.answer(&reactor, "/tls/roots", 8).await;
        assert_eq!(
            text, "roots=0 ",
            "the machine root store is the served file: empty"
        );
    }

    #[tokio::test]
    async fn the_root_store_lists_the_bundle(runtime: GuestRuntime) {
        let reactor = reactor_serving(
            vec![],
            bundle(&[
                tls_material::ROOT_CERTIFICATE,
                tls_material::STRANGER_CERTIFICATE,
            ]),
        );
        let mut guest = guest(&reactor, runtime);
        let text = guest.answer(&reactor, "/tls/roots", 8).await;
        let mut expected = [
            root_thumbprint(tls_material::ROOT_CERTIFICATE),
            root_thumbprint(tls_material::STRANGER_CERTIFICATE),
        ];
        expected.sort();
        assert_eq!(
            text,
            format!("roots=2 {}", expected.join(",")),
            "X509Store(Root) is the served bundle"
        );
    }

    #[tokio::test]
    async fn a_slice_clock_advance_is_the_same_advance_on_the_guests_stopwatch(runtime: GuestRuntime) {
        let reactor = reactor_serving(vec![], bundle(&[]));
        let mut guest = guest(&reactor, runtime);
        let before = guest.answer(&reactor, "/tls/clock", 8).await;
        reactor.context.begin_slice(CLOCK_2030_MS + RECHECK_MS, 2);
        let after = guest.answer(&reactor, "/tls/clock", 8).await;
        let field = |text: &str, name: &str| -> i64 {
            text.split(' ')
                .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| panic!("the clock route reports {name}: {text}"))
        };
        let frequency = field(&before, "frequency");
        let elapsed_ms =
            (field(&after, "timestamp") - field(&before, "timestamp")) * 1_000 / frequency;
        assert_eq!(
            elapsed_ms as u64, RECHECK_MS,
            "Stopwatch time is the slice clock: {before} / {after}"
        );
        assert_eq!(
            (field(&after, "tick") - field(&before, "tick")) as u64,
            RECHECK_MS,
            "TickCount64 is the slice clock too: {before} / {after}"
        );
    }

    #[tokio::test]
    async fn the_served_bundles_timestamps_follow_its_content(runtime: GuestRuntime) {
        let reactor = reactor_serving(vec![], bundle(&[tls_material::ROOT_CERTIFICATE]));
        let mut guest = guest(&reactor, runtime);
        let before = guest.answer(&reactor, "/tls/bundle-stat", 8).await;
        guest.serve_platform_files(bundle(&[tls_material::STRANGER_CERTIFICATE]));
        reactor.context.begin_slice(CLOCK_2030_MS + RECHECK_MS, 2);
        let after = guest.answer(&reactor, "/tls/bundle-stat", 8).await;
        assert_ne!(before, after, "a different bundle has a different timestamp for the file and its directory: {before} / {after}");
        let dir = |text: &str| text.split(' ').next().map(str::to_owned);
        assert_ne!(
            dir(&before),
            dir(&after),
            "the directory's timestamp follows its file: {before} / {after}"
        );
    }

    #[tokio::test]
    async fn a_removed_root_stays_trusted_until_the_stores_next_recheck(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let (reactor, mut guest) = validated_then_rotated(
            runtime,
            peer,
            &[tls_material::ROOT_CERTIFICATE],
            &[tls_material::STRANGER_CERTIFICATE],
        )
        .await;
        // Less than five seconds of guest time since the store was last
        // checked: the cached roots still serve this validation.
        reactor.context.begin_slice(CLOCK_2030_MS + 1_000, 2);
        let text = guest
            .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
            .await;
        assert!(
            text.starts_with("protocol=Tls1"),
            "within the recheck interval a validation still uses the roots the store cached: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_removed_root_is_untrusted_by_an_existing_guest_after_the_recheck(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let (reactor, mut guest) = validated_then_rotated(
            runtime,
            peer,
            &[tls_material::ROOT_CERTIFICATE],
            &[tls_material::STRANGER_CERTIFICATE],
        )
        .await;
        reactor.context.begin_slice(CLOCK_2030_MS + RECHECK_MS, 2);
        let text = guest
            .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("UntrustedRoot"),
            "five seconds of guest time after the last check, the next validation reloads the served file and the removed root is gone: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        let text = guest.answer(&reactor, "/tls/roots", 8).await;
        assert_eq!(
            text,
            format!(
                "roots=1 {}",
                root_thumbprint(tls_material::STRANGER_CERTIFICATE)
            ),
            "the cached runtime state is the rotated bundle"
        );
    }

    #[tokio::test]
    async fn an_added_root_is_trusted_by_an_existing_guest_after_the_recheck(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let reactor = reactor_serving(vec![peer], bundle(&[tls_material::STRANGER_CERTIFICATE]));
        let mut guest = guest(&reactor, runtime);
        let text = guest
            .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
            .await;
        assert!(
            text.contains("UntrustedRoot"),
            "before the rotation the root is not in the bundle: {text}"
        );
        guest.serve_platform_files(bundle(&[
            tls_material::STRANGER_CERTIFICATE,
            tls_material::ROOT_CERTIFICATE,
        ]));
        reactor.context.begin_slice(CLOCK_2030_MS + RECHECK_MS, 2);
        let text = guest
            .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
            .await;
        assert!(
            text.starts_with("protocol=Tls1"),
            "after the recheck the added root is trusted: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_rollback_to_the_previous_bundle_is_observed_like_any_change(runtime: GuestRuntime) {
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let (reactor, mut guest) = validated_then_rotated(
            runtime,
            peer,
            &[tls_material::ROOT_CERTIFICATE],
            &[tls_material::STRANGER_CERTIFICATE],
        )
        .await;
        reactor.context.begin_slice(CLOCK_2030_MS + RECHECK_MS, 2);
        let text = guest
            .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
            .await;
        assert!(
            text.contains("UntrustedRoot"),
            "the rotation removed the root: {text}"
        );
        // Back to the earlier bytes: the content-derived timestamp is the
        // earlier one again, which differs from the current, so it is seen.
        guest.serve_platform_files(bundle(&[tls_material::ROOT_CERTIFICATE]));
        reactor
            .context
            .begin_slice(CLOCK_2030_MS + 2 * RECHECK_MS, 3);
        let text = guest
            .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
            .await;
        assert!(
            text.starts_with("protocol=Tls1"),
            "the rollback restores trust at the next recheck: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_removed_root_is_untrusted_by_a_parked_guest_once_restored(runtime: GuestRuntime) {
        // Start with cached roots, rotate the served bundle, then connect
        // after the recheck interval. Every parked wake restores the saved
        // instance before this handshake reports its result.
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let (reactor, mut guest) = validated_then_rotated(
            runtime,
            peer,
            &[tls_material::ROOT_CERTIFICATE],
            &[tls_material::ROOT_CERTIFICATE],
        )
        .await;
        guest.serve_platform_files(bundle(&[tls_material::STRANGER_CERTIFICATE]));
        reactor.context.begin_slice(CLOCK_2030_MS + RECHECK_MS, 2);
        let outcome = guest.start(&system_route(peer, "leaf.acceptance.test"));
        assert_eq!(
            outcome.completion,
            wisp_engine::Completion::Parked,
            "the connect parks: {outcome:?}"
        );
        let text = guest.finish(&reactor, 32).await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("UntrustedRoot"),
            "restored after the rotation, the guest validates against the served bundle: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }

    #[tokio::test]
    async fn a_guest_activated_from_an_image_holding_old_roots_reloads_after_the_recheck(runtime: GuestRuntime) {
        // A memory image that already holds the cached store (as a template
        // warmed at preparation would) activates under a rotated bundle.
        let peer = tls_peer(Certificate::Leaf, Behaviour::Echo).await;
        let (reactor, warmed) = validated_then_rotated(
            runtime,
            peer,
            &[tls_material::ROOT_CERTIFICATE],
            &[tls_material::STRANGER_CERTIFICATE],
        )
        .await;
        let mut fresh = warmed.activated_from_image(&reactor);
        reactor.context.begin_slice(CLOCK_2030_MS + RECHECK_MS, 2);
        let text = fresh
            .answer(&reactor, &system_route(peer, "leaf.acceptance.test"), 32)
            .await;
        assert!(
            text.starts_with("AuthenticationException: ") && text.contains("UntrustedRoot"),
            "the image's cached roots are rechecked against the served file on first use: {text}; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }
}
