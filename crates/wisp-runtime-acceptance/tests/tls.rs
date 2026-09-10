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

/// The pattern the guest sends: `length` bytes, as hex.
fn pattern(length: usize) -> String {
    (0..length)
        .map(|index| format!("{:02X}", (index % 251) as u8))
        .collect()
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
