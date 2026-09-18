//! Outbound TCP through the source-built socket engine (runtime patch 007):
//! readiness comes from the pollables libc registers for a socket, observed
//! through libc's public poll rather than its private descriptor layout, and
//! every connection state the peer can produce reaches the ordinary managed
//! exception across destroyed Stores.

#[macro_use]
mod common;
#[path = "common/admitted.rs"]
mod admitted;
#[path = "common/fixed_dns.rs"]
mod fixed_dns;
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
use isoplane_engine::Completion;
use isoplane_harness::EmittedGuestLogs as _;
use no_egress::reactor_without_egress;
use peers::{
    fnv1a, gated_peer, pattern, peer, peer_with_receive_buffer, reactor_for_peers,
    reactor_for_peers_with_send_buffer, slow_drain_peer, Peer,
};
use refusing::refusing_endpoint;

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
            answer, "70696E67",
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
    /// Timeouts govern blocking calls, which an Isoplane socket never makes; they are
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
    /// An Isoplane socket never blocks; the synchronous socket calls either refuse
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
            text, "zero=0 then 70696E67",
            "the zero-byte receive completes with nothing consumed and the real read gets the bytes; logs: {:?}",
            reactor.context.emitted_logs()
        );
    }
}

in_both_runtimes! {
    /// Enabled linger asks for a close-with-reset the host never performs;
    /// the runtime refuses it before any host call, as upstream .NET does on
    /// WASI, and the socket keeps working.
    #[tokio::test]
    async fn enabling_linger_is_refused_as_unsupported_and_the_socket_stays_usable(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(30, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/linger/{}/{}/1/5", echo.ip(), echo.port()),
                8,
            )
            .await;
        assert_eq!(
            answer, "set=PlatformNotSupportedException get=False/0 echo=77697370",
            "enabling linger must be refused as unsupported, read back as disabled, and leave the socket usable"
        );
    }
}

in_both_runtimes! {
    /// Disabled linger is the platform's only state: the SDK shim accepts
    /// setting it as the no-op it is (libc itself has no SO_LINGER), and the
    /// socket keeps working and reads back as not lingering.
    #[tokio::test]
    async fn disabling_linger_is_accepted_as_a_no_op_and_the_socket_stays_usable(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(31, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/linger/{}/{}/0/0", echo.ip(), echo.port()),
                8,
            )
            .await;
        assert_eq!(
            answer, "set=ok get=False/0 echo=77697370",
            "disabling linger must succeed as a no-op, read back as disabled, and leave the socket usable"
        );
    }
}

in_both_runtimes! {
    /// A negative timeout and a negative linger time are refused by managed
    /// argument checks; a NoDelay value too short for the option reaches the
    /// host shim and is refused as an invalid argument. Each leaves the value
    /// it targeted in place. The invalid-argument refusal also marks the
    /// managed Socket disconnected, as .NET does on every platform, while the
    /// host connection still carries bytes.
    #[tokio::test]
    async fn an_invalid_option_value_is_refused_and_leaves_the_previous_value_in_place(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(32, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/invalid-value/{}/{}", echo.ip(), echo.port()),
                8,
            )
            .await;
        assert_eq!(
            answer,
            "ArgumentOutOfRangeException ArgumentException InvalidArgument nodelay=True timeout=1500 connected=False echo=77697370",
            "every invalid value must be refused with its own error, NoDelay and the timeout must keep their values, and the connection must still echo"
        );
    }
}

in_both_runtimes! {
    /// Buffer sizes and keep-alive reach the host and are refused: they
    /// neither round-trip in the guest nor change the host socket. The
    /// refusal leaves NoDelay, real host state, in place and the socket usable.
    #[tokio::test]
    async fn an_unsupported_option_is_refused_and_leaves_the_socket_usable_with_its_state(runtime: GuestRuntime) {
        let echo = peer(Peer::Echo).await;
        let reactor = reactor_for_peers(33, vec![echo]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/unsupported-option/{}/{}", echo.ip(), echo.port()),
                8,
            )
            .await;
        assert_eq!(
            answer,
            "OperationNotSupported OperationNotSupported OperationNotSupported nodelay=True echo=77697370",
            "setting the receive buffer, reading the send buffer and enabling keep-alive must each be refused as unsupported without disturbing NoDelay or the connection"
        );
    }
}

/// How a test resumes its parked guest.
#[derive(Clone, Copy)]
enum WakePath {
    /// Restore the saved instance and resume.
    Direct,
    /// Restore the saved instance for the alternate scenario.
    ThroughDelta,
}

async fn wake_as(
    guest: &mut isoplane_harness::ReturnedGuest,
    reactor: &isoplane_harness::Reactor,
    wake: WakePath,
) -> isoplane_engine::DispatchOutcome {
    match wake {
        WakePath::Direct => guest.wake(reactor).await,
        WakePath::ThroughDelta => guest.wake(reactor).await,
    }
}

/// One write of this many bytes, against socket buffers of
/// [`SMALL_SOCKET_BUFFER_BYTES`] on both ends, drained by the peer in
/// [`DRAIN_CHUNK_BYTES`] steps: at most two buffers plus one chunk can move
/// per wake, so the write must park at least (512 - 64) / 32 = 14 times.
const BACKPRESSURE_PAYLOAD_BYTES: usize = 512 * 1024;
const SMALL_SOCKET_BUFFER_BYTES: u32 = 16 * 1024;
const DRAIN_CHUNK_BYTES: usize = 32 * 1024;
const FEWEST_BACKPRESSURE_PARKS: usize = 8;

/// A send larger than both socket buffers to a peer that drains one chunk
/// per park: the guest parks on writability at every full buffer, and the
/// peer's digest proves every byte arrived once, in order.
async fn a_large_send_under_backpressure(runtime: GuestRuntime, owner: u64, wake: WakePath) {
    isoplane_harness::LogCapture::global();
    let (sink, drain, drained) =
        slow_drain_peer(SMALL_SOCKET_BUFFER_BYTES, DRAIN_CHUNK_BYTES).await;
    let reactor = reactor_for_peers_with_send_buffer(owner, vec![sink], SMALL_SOCKET_BUFFER_BYTES);
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    let outcome = guest.start(&format!(
        "/sockets/backpressure/{}/{}/{BACKPRESSURE_PAYLOAD_BYTES}",
        sink.ip(),
        sink.port()
    ));
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "the connect parks: {outcome:?}"
    );
    let mut outcome = wake_as(&mut guest, &reactor, wake).await;
    assert_eq!(
        outcome.completion,
        Completion::Parked,
        "a write larger than the socket buffers must park on writability while the peer is not draining: {outcome:?}; logs: {:?}",
        reactor.context.emitted_logs()
    );
    assert_eq!(
        reactor.futures.pending_total(reactor.context.instance),
        2,
        "the parked write holds the socket's two readiness wakes: output to resume the write, and input because the engine polls both directions"
    );
    let mut parks = 1;
    while outcome.completion == Completion::Parked {
        assert!(
            parks < 64,
            "the write must finish within 64 parks; logs: {:?}",
            reactor.context.emitted_logs()
        );
        drain
            .send(())
            .await
            .expect("the slow peer must take a drain permit");
        outcome = wake_as(&mut guest, &reactor, wake).await;
        parks += 1;
    }
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "{outcome:?}; logs: {:?}",
        reactor.context.emitted_logs()
    );
    assert_eq!(
        reactor.response_text(1),
        format!("sent {BACKPRESSURE_PAYLOAD_BYTES}"),
        "the write must report every byte accepted"
    );
    assert!(
        parks >= FEWEST_BACKPRESSURE_PARKS,
        "with {SMALL_SOCKET_BUFFER_BYTES}-byte buffers drained {DRAIN_CHUNK_BYTES} bytes per wake, a {BACKPRESSURE_PAYLOAD_BYTES}-byte write cannot complete in fewer than {FEWEST_BACKPRESSURE_PARKS} parks; it took {parks}"
    );
    assert_eq!(
        reactor.futures.pending_total(reactor.context.instance),
        0,
        "the finished write and its disposed socket must leave no registration"
    );
    drop(drain);
    let report = tokio::time::timeout(isoplane_harness::WAKE_DEADLINE, drained)
        .await
        .expect("the peer must drain the rest of the stream before the test deadline")
        .expect("the peer must report what it drained");
    assert_eq!(
        report.bytes, BACKPRESSURE_PAYLOAD_BYTES,
        "every byte of the write must reach the peer"
    );
    assert_eq!(
        report.digest,
        fnv1a(&pattern(BACKPRESSURE_PAYLOAD_BYTES)),
        "the bytes must arrive in order, none duplicated by a retried partial send"
    );
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_large_send_to_a_slow_reader_parks_on_writability_and_delivers_every_byte_in_order(runtime: GuestRuntime) {
        a_large_send_under_backpressure(runtime, 34, WakePath::Direct).await;
    }
}

in_both_runtimes! {
    /// The partially sent buffer and send-loop position survive store restoration.
    #[tokio::test]
    async fn a_large_send_under_backpressure_survives_store_restoration_at_every_park(runtime: GuestRuntime) {
        a_large_send_under_backpressure(runtime, 35, WakePath::ThroughDelta).await;
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_large_stream_is_received_intact_through_small_guest_buffers(runtime: GuestRuntime) {
        const STREAM_BYTES: usize = 300 * 1024;
        let source = peer(Peer::Pattern(STREAM_BYTES)).await;
        let reactor = reactor_for_peers(36, vec![source]);
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let answer = guest
            .answer(
                &reactor,
                &format!("/sockets/drain/{}/{}/1024", source.ip(), source.port()),
                512,
            )
            .await;
        assert_eq!(
            answer,
            format!("received {STREAM_BYTES} fnv={:016x}", fnv1a(&pattern(STREAM_BYTES))),
            "reading a large stream 1 KiB at a time must deliver every byte once, in order, up to the peer's EOF"
        );
        assert_eq!(
            reactor.futures.pending_total(reactor.context.instance),
            0,
            "a stream read to EOF must leave no registration"
        );
    }
}

/// A synchronous socket call the platform cannot serve without blocking:
/// it must fail in the dispatch that made it, without parking. The connect
/// before it takes at most one wake, and none when the loopback peer
/// accepts before the guest first polls.
async fn a_synchronous_call_fails_fast(
    runtime: GuestRuntime,
    owner: u64,
    route: &str,
    arguments: &str,
    what: &str,
) {
    isoplane_harness::LogCapture::global();
    let silent = peer_with_receive_buffer(Peer::Silent, SMALL_SOCKET_BUFFER_BYTES).await;
    let reactor =
        reactor_for_peers_with_send_buffer(owner, vec![silent], SMALL_SOCKET_BUFFER_BYTES);
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    let mut outcome = guest.start(&format!(
        "/sockets/{route}/{}/{}{arguments}",
        silent.ip(),
        silent.port()
    ));
    let mut wakes = 0;
    while outcome.completion == Completion::Parked {
        assert_eq!(
            wakes, 0,
            "{what} must fail in the dispatch that made it, never park waiting for the peer: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        outcome = guest.wake(&reactor).await;
        wakes += 1;
    }
    assert_eq!(
        outcome.completion,
        Completion::Done,
        "{outcome:?}; logs: {:?}",
        reactor.context.emitted_logs()
    );
    assert_eq!(
        reactor.response_text(1),
        "PlatformNotSupportedException",
        "{what} must be refused as unsupported on this platform, not hang or trap"
    );
    assert_eq!(
        reactor.futures.pending_total(reactor.context.instance),
        0,
        "{what} must leave no wake behind"
    );
}

in_both_runtimes! {
    /// The buffers on both ends are far smaller than the payload, so a
    /// blocking send would have to wait for the silent peer.
    #[tokio::test]
    async fn a_synchronous_send_exceeding_the_socket_buffer_fails_fast_instead_of_blocking(runtime: GuestRuntime) {
        a_synchronous_call_fails_fast(
            runtime,
            37,
            "sync-send-large",
            &format!("/{BACKPRESSURE_PAYLOAD_BYTES}"),
            "a synchronous Send larger than the socket buffers",
        )
        .await;
    }
}

in_both_runtimes! {
    /// ReceiveTimeout round-trips as a property; it never times a receive
    /// out, because a synchronous receive is refused before it could wait.
    /// The reactor's clock is not advanced: nothing here may depend on time.
    #[tokio::test]
    async fn a_receive_timeout_does_not_make_a_synchronous_receive_wait(runtime: GuestRuntime) {
        a_synchronous_call_fails_fast(
            runtime,
            38,
            "timed-receive",
            "",
            "a synchronous Receive with a timeout set and nothing to read",
        )
        .await;
    }
}

async fn configured_tcp_client(runtime: GuestRuntime, mode: &str, ipv6: bool) {
    isoplane_harness::LogCapture::global();
    let endpoint = if ipv6 {
        peers::ipv6_peer(Peer::Echo).await
    } else {
        peer(Peer::Echo).await
    };
    let guard = if mode == "mixed" {
        let socket =
            tokio::net::TcpSocket::new_v4().expect("the refused IPv4 address must be reservable");
        socket
            .bind((std::net::Ipv4Addr::LOCALHOST, endpoint.port()).into())
            .expect("the IPv4 control port must be reserved without listening");
        Some(socket)
    } else {
        None
    };
    let mut admitted = vec![endpoint];
    if !ipv6 {
        admitted.push(
            (
                std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
                endpoint.port(),
            )
                .into(),
        );
    }
    if guard.is_some() {
        admitted.push((std::net::Ipv4Addr::LOCALHOST, endpoint.port()).into());
        admitted.push(
            (
                std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
                endpoint.port(),
            )
                .into(),
        );
    }
    let mut reactor = if mode == "dns" {
        isoplane_harness::Reactor::new(
            isoplane_host::InstanceId::alloc(),
            admitted::admitting(
                admitted,
                Box::new(fixed_dns::FixedDns(vec![
                    std::net::Ipv4Addr::LOCALHOST.into()
                ])),
            ),
        )
    } else {
        reactor_for_peers(880, admitted)
    };
    if mode == "dns" || mode == "literal" {
        let allowed = if mode == "dns" {
            "approved.test".to_owned()
        } else {
            endpoint.ip().to_string()
        };
        std::sync::Arc::get_mut(&mut reactor.context)
            .expect("the context is not shared before guest creation")
            .allowlist = Some(vec![allowed]);
    }
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    let answer = guest
        .answer(
            &reactor,
            &format!(
                "/sockets/client/{mode}/{}/{}",
                endpoint.ip(),
                endpoint.port()
            ),
            30,
        )
        .await;
    let expected = if mode == "cancelled" {
        "cancelled"
    } else if mode == "only-v6" {
        "IPv6-only refused mapped IPv4"
    } else if mode == "only-v4" {
        "IPv4-only refused IPv6"
    } else if mode == "connected-option" {
        "connected mode refused"
    } else {
        "echo; same client; configuration retained"
    };
    assert_eq!(answer, expected, "TcpClient must retain its public Socket and configured options while connecting through the selected address family; logs: {:?}", reactor.context.emitted_logs());
    released(&reactor).await;
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_default_tcp_client_connects_to_ipv4_with_its_configuration(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "address", false).await;
    }
    #[tokio::test]
    async fn a_default_tcp_client_connects_to_ipv6_with_its_configuration(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "address", true).await;
    }
    #[tokio::test]
    async fn a_default_tcp_client_connects_to_an_ipv6_string(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "string", true).await;
    }
    #[tokio::test]
    async fn a_default_tcp_client_connects_to_an_ipv6_address_list(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "list", true).await;
    }
    #[tokio::test]
    async fn a_default_tcp_client_retries_ipv6_after_ipv4_refusal(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "mixed", true).await;
    }
    #[tokio::test]
    async fn an_explicit_ipv6_tcp_client_keeps_its_family_and_configuration(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "explicit", true).await;
    }
    #[tokio::test]
    async fn a_cancelled_default_tcp_client_connect_keeps_its_socket(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "cancelled", true).await;
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn an_ipv6_only_tcp_client_refuses_an_ipv4_mapped_destination(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "only-v6", false).await;
    }
    #[tokio::test]
    async fn a_tcp_clients_dual_socket_option_enables_ipv4_mapped_connect(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "option-mapped", false).await;
    }
    #[tokio::test]
    async fn a_default_tcp_client_keeps_an_exposed_socket_handle_when_connecting(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "exposed", false).await;
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_default_tcp_client_retains_an_approved_ipv4_dns_answers_authority(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "dns", false).await;
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn an_explicit_ipv4_tcp_client_refuses_an_ipv6_destination(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "only-v4", true).await;
    }

    #[tokio::test]
    async fn a_connected_socket_refuses_changing_its_family_mode(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "connected-option", true).await;
    }
}

in_both_runtimes! {
    #[tokio::test]
    async fn a_default_tcp_client_retains_an_allowlisted_ipv4_literals_authority(runtime: GuestRuntime) {
        configured_tcp_client(runtime, "literal", false).await;
    }
}

async fn socket_apm(runtime: GuestRuntime, mode: &str) {
    let echo = peer(Peer::Echo).await;
    let reactor = reactor_for_peers(934, vec![echo]);
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    let started = guest.start(&format!(
        "/sockets/apm/{mode}/{}/{}",
        echo.ip(),
        echo.port()
    ));
    assert_eq!(
        started.completion,
        Completion::Parked,
        "APM fixture must first park while connecting"
    );
    let answer = guest.finish(&reactor, 64).await;
    assert_eq!(answer, "APM callbacks restored; unfinished End refused", "task-backed APM callbacks, state, completed End calls and unfinished-wait refusal must survive Store restoration");
    released(&reactor).await;
}
in_both_runtimes! {
    #[tokio::test]
    async fn network_stream_apm_callbacks_survive_store_restoration(runtime: GuestRuntime) { socket_apm(runtime, "stream").await; }
    #[tokio::test]
    async fn socket_apm_callbacks_survive_store_restoration(runtime: GuestRuntime) { socket_apm(runtime, "socket").await; }
    #[tokio::test]
    async fn socket_apm_error_code_overloads_survive_store_restoration(runtime: GuestRuntime) { socket_apm(runtime, "socket-error").await; }
    #[tokio::test]
    async fn socket_apm_vector_callbacks_survive_store_restoration(runtime: GuestRuntime) { socket_apm(runtime, "vector").await; }
    #[tokio::test]
    async fn socket_apm_vector_error_code_overloads_survive_store_restoration(runtime: GuestRuntime) { socket_apm(runtime, "vector-error").await; }
}
