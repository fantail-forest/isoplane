//! Outbound TCP through the source-built socket engine (runtime patch 007):
//! readiness comes from the pollables libc registers for a socket, observed
//! through libc's public poll rather than its private descriptor layout, and
//! every connection state the peer can produce reaches the ordinary managed
//! exception across destroyed Stores.

#[macro_use]
mod common;
#[path = "common/admitted.rs"]
mod admitted;
#[path = "common/no_dns.rs"]
mod no_dns;
#[path = "common/no_egress.rs"]
mod no_egress;
#[path = "common/peers.rs"]
mod peers;
#[path = "common/refusing.rs"]
mod refusing;

use admitted::released;
use common::{guest, GuestRuntime};
use no_egress::reactor_without_egress;
use peers::{gated_peer, peer, reactor_for_peers, Peer};
use refusing::refusing_endpoint;
use wisp_engine::Completion;
use wisp_harness::EmittedGuestLogs as _;

in_both_runtimes! {
    #[tokio::test]
    async fn a_connection_parks_while_connecting_and_resumes_connected(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(10, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/sockets/connect/{}/{}", echo.ip(), echo.port()));
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "ConnectAsync must park on the socket's own pollable while connecting: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            1,
            "a connecting socket registers one wake source"
        );
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "{outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        let answer = reactor.response_text(1);
        assert!(
            answer.starts_with("connected ") && answer.ends_with(&format!("->{echo}")),
            "the connected socket must report real local and remote endpoints: {answer}"
        );
        assert_eq!(
            reactor.runtime().services.egress.connections(),
            0,
            "disposing the socket must release its host connection"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_stream_echoes_bytes_through_input_and_output_readiness(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(11, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/echo/{}/{}/77697370", echo.ip(), echo.port()),
                8,
            )
            .await;
        assert_eq!(
            answer, "77697370",
            "every byte written must come back through the read"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "a finished transfer must leave no registration"
        );
    }
}

in_both_runtimes! {
    /// A parked read survives Store restoration: the guest's captured memory is
    /// restored between the park and the wake that delivers the bytes.
    #[tokio::test]
    async fn a_parked_read_survives_a_store_restoration_delta_round_trip(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(25, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let mut outcome = guest.start(&format!(
            "/sockets/echo/{}/{}/77697370",
            echo.ip(),
            echo.port()
        ));
        let mut wakes = 0;
        while outcome.completion == Completion::Parked {
            wakes += 1;
            assert!(
                wakes <= 8,
                "the echo must finish within a few wakes: {outcome:?}"
            );
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "{outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.response_text(1),
            "77697370",
            "the bytes must reach the read that was parked across the Store restoration"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn two_sockets_in_flight_observe_their_own_pollables(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(12, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/two/{}/{}/0102/0304", echo.ip(), echo.port()),
                12,
            )
            .await;
        assert_eq!(
            answer, "0102+0304",
            "each socket's readiness must be its own; a poll capture must not cross sockets"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_refused_connection_is_reported_inline_as_connection_refused(runtime: GuestRuntime) {
        let (_reservation, refused) = refusing_endpoint();
        let reactor = reactor_for_peers(13, vec![refused]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/refused/{}/{}", refused.ip(), refused.port()),
                4,
            )
            .await;
        assert_eq!(
            answer, "ConnectionRefused",
            "libc reports a failed connect as immediately ready with no pollable; it must reach SocketException"
        );
        assert_eq!(
            reactor.runtime().services.egress.connections(),
            0,
            "a refused connection must release its quota"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "a refused connection must leave no wake behind"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_read_receives_clean_eof_when_the_peer_closes(runtime: GuestRuntime) {
        let eof = peer(Peer::Eof).await;
        let reactor = reactor_for_peers(14, vec![eof]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/eof/{}/{}", eof.ip(), eof.port()),
                6,
            )
            .await;
        assert_eq!(
            answer, "eof",
            "a closed peer must complete the read with zero bytes"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_read_receives_a_reset_as_a_socket_error(runtime: GuestRuntime) {
        let reset = peer(Peer::Reset).await;
        let reactor = reactor_for_peers(15, vec![reset]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/reset/{}/{}", reset.ip(), reset.port()),
                6,
            )
            .await;
        assert_eq!(
            answer, "ConnectionReset",
            "an aborted connection must reach the read as a reset SocketException"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_half_closed_socket_keeps_its_receive_direction(runtime: GuestRuntime) {
        let half = peer(Peer::HalfClose).await;
        let reactor = reactor_for_peers(16, vec![half]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/half/{}/{}", half.ip(), half.port()),
                6,
            )
            .await;
        assert_eq!(
            answer, "77697370",
            "after Shutdown(Send) the peer's answer must still arrive through the read pollable"
        );
    }
}

in_both_runtimes! {
    #[tokio::test(start_paused = true)]
    async fn cancelling_a_read_releases_the_connection(runtime: GuestRuntime) {
        let silent = peer(Peer::Silent).await;
        let reactor = reactor_for_peers(17, vec![silent]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let mut outcome = guest.start(&format!(
            "/sockets/cancel/{}/{}/50",
            silent.ip(),
            silent.port()
        ));
        // connect, then the read that waits on the peer
        for _ in 0..2 {
            assert_eq!(outcome.completion, Completion::Parked, "{outcome:?}");
            tokio::task::yield_now().await;
            if reactor.futures.pending_total(reactor.context.instance) == 2 {
                break;
            }
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            2,
            "the pending read and its cancellation timer must both be host-owned"
        );
        tokio::time::advance(std::time::Duration::from_millis(60)).await;
        reactor.context.begin_slice(61, 1);
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "{outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.response_text(1),
            "cancelled",
            "the read must observe its cancellation"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "cancellation must release the read's wake and the timer"
        );
        tokio::time::resume();
        released(&reactor).await;
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_disposed_socket_drops_its_registration_before_the_next_poll(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(18, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!(
                    "/sockets/dispose-then-connect/{}/{}/0a0b",
                    echo.ip(),
                    echo.port()
                ),
                10,
            )
            .await;
        assert_eq!(
            answer, "0A0B",
            "a hook whose socket was disposed must not break the next socket's readiness"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "neither socket may leave a registration"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn no_delay_is_real_host_state_before_and_after_connecting(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(19, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/nodelay/{}/{}", echo.ip(), echo.port()),
                4,
            )
            .await;
        assert_eq!(
            answer, "True True False",
            "NoDelay set before connecting must be retained on the host socket and clearable after"
        );
    }
}

in_both_runtimes! {
    /// Timeouts govern blocking calls, which a WISP socket never makes; they are
    /// stored like any other option rather than refused, as they are for
    /// asynchronous use on every other platform.
    #[tokio::test]
    async fn socket_and_stream_timeouts_are_stored_options(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(21, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/timeouts/{}/{}", echo.ip(), echo.port()),
                4,
            )
            .await;
        assert_eq!(
            answer, "1500 2500 3500 4500 5500",
            "Socket, TcpClient and NetworkStream timeouts must round-trip instead of throwing"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_send_into_a_reset_connection_is_a_socket_error_naming_the_reset(runtime: GuestRuntime) {
        let reset = peer(Peer::Reset).await;
        let reactor = reactor_for_peers(22, vec![reset]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!(
            "/sockets/reset-send/{}/{}/20",
            reset.ip(),
            reset.port()
        ));
        let mut outcome = outcome;
        let mut wakes = 0;
        while outcome.completion == Completion::Parked {
            wakes += 1;
            assert!(
                wakes <= 8,
                "the send must fail within a few wakes: {outcome:?}"
            );
            reactor.context.begin_slice(1 + 25 * wakes, 1);
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "{outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        let answer = reactor.response_text(1);
        assert!(
            answer == "ConnectionReset" || answer == "Shutdown",
            "a send into a reset connection must name the reset or the closed pipe, not a generic error: {answer}"
        );
    }
}

in_both_runtimes! {
    /// A WISP socket never blocks; the synchronous socket calls either refuse
    /// explicitly (Connect) or report the ordinary would-block error, never a
    /// trap or a hang.
    #[tokio::test]
    async fn synchronous_socket_calls_refuse_or_report_would_block_instead_of_blocking(runtime: GuestRuntime) {
        let silent = peer(Peer::Silent).await;
        let reactor = reactor_for_peers(23, vec![silent]);
        let mut guest = guest(&reactor, runtime);
        let refused: &[&str] = &["PlatformNotSupportedException", "WouldBlock"];
        let refused_or_sent: &[&str] = &["PlatformNotSupportedException", "WouldBlock", "sent 4"];
        for (index, (path, accepted, what)) in [
            ("/sockets/sync-connect", refused, "a synchronous Connect"),
            (
                "/sockets/sync-receive",
                refused,
                "a synchronous Receive with nothing to read",
            ),
            (
                "/sockets/sync-send",
                refused_or_sent,
                "a synchronous Send of a few bytes",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            reactor
                .context
                .begin_slice(index as u64 + 1, index as u64 + 1);
            let answer = guest
                .answer(
                    &reactor,
                    &format!("{path}/{}/{}", silent.ip(), silent.port()),
                    4,
                )
                .await;
            assert!(
                accepted.iter().any(|expected| answer.contains(expected)),
                "{what} must complete, refuse or report would-block, never block: {answer}"
            );
        }
    }
}

in_both_runtimes! {
    /// Socket.Poll waits inside the call; it is refused explicitly rather than
    /// trapped by the host's refusal to block.
    #[tokio::test]
    async fn a_synchronous_poll_is_refused_explicitly(runtime: GuestRuntime) {
        let silent = peer(Peer::Silent).await;
        let reactor = reactor_for_peers(24, vec![silent]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/sync-poll/{}/{}", silent.ip(), silent.port()),
                4,
            )
            .await;
        assert!(
            answer == "PlatformNotSupportedException" || answer == "SocketException",
            "Socket.Poll must be refused with an exception, not a trap: {answer}"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn listening_and_datagram_sockets_are_refused_with_socket_errors(runtime: GuestRuntime) {
        let reactor = reactor_without_egress(20);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        assert_eq!(
            guest.answer(&reactor, "/sockets/listen", 2).await,
            "AccessDenied",
            "a guest listener must be refused, not trapped"
        );
        reactor.context.begin_slice(2, 2);
        assert_eq!(
            guest.answer(&reactor, "/sockets/udp", 2).await,
            "OperationNotSupported",
            "UDP must be refused as unsupported, not trapped"
        );
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_zero_byte_receive_parks_until_data_arrives_and_leaves_it_for_the_read(runtime: GuestRuntime) {
        let (peer, gate) = gated_peer().await;
        let reactor = reactor_for_peers(90, vec![peer]);
        let mut guest = guest(&reactor, runtime);
        let outcome = guest.start(&format!("/sockets/zero-byte/{}/{}", peer.ip(), peer.port()));
        assert_eq!(outcome.completion, Completion::Parked, "the connect parks: {outcome:?}");
        let outcome = guest.wake(&reactor).await;
        assert_eq!(
            outcome.completion,
            Completion::Parked,
            "a zero-byte receive with nothing to read parks on socket readiness: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            1,
            "the parked zero-byte receive holds exactly one host future; logs: {:?}",
            reactor.context.emitted_logs()
        );
        gate.notify_one();
        let text = guest.finish(&reactor, 8).await;
        assert_eq!(
            text, "zero=0 then 77697370",
            "the zero-byte receive completes with nothing consumed and the real read gets the bytes; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }
}
