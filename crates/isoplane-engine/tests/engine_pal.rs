//! The WASI PAL adapter and the native I/O packets exercised through the
//! full engine.

mod common;

use common::*;
use isoplane_engine::{Completion, InstanceOps, OccupancyBudget};

/// A `DispatchRecord` with nothing interesting in it.
fn http_dispatch(id: u64) -> isoplane_abi::dispatch::DispatchRecord {
    use isoplane_abi::dispatch::*;
    DispatchRecord {
        dispatch_id: id,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/x".into(),
            path: "/x".into(),
            query: vec![],
            headers: vec![("host".into(), "example.test".into())],
        }),
        cancel_future: 0,
        occupancy_budget_ms: 30_000,
        pending_futures_limit: 64,
        now_ms: 1_700_000_000_000,
    }
}

#[test]
fn a_module_with_wasi_imports_instantiates_through_the_pal_adapter() {
    let e = test_engine();
    let wasm = guest(
        4,
        r#"
  (import "wasi:clocks/monotonic-clock@0.2.0" "subscribe-duration" (func (param i64) (result i32)))
  (import "wasi:io/poll@0.2.0" "poll" (func (param i32 i32 i32)))
  (import "wasi:io/poll@0.2.0" "[resource-drop]pollable" (func (param i32)))
"#,
        "",
        "(i64.const 0)",
        DONE,
    );
    let t = e
        .load_template(&wasm)
        .expect("a whitelisted wasi: surface must instantiate");

    let mut i = activate(&e, &t);
    assert_eq!(
        i.dispatch(&http_dispatch(1), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "the module dispatches"
    );
}

mod native_readiness {
    use super::common::{guest, test_engine, DONE};
    use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest};
    use isoplane_abi::native_io::{Request, ResourceHandle, Response};
    use isoplane_engine::{Completion, HostBindings, ImportCall, InstanceOps, OccupancyBudget};

    #[derive(Default)]
    struct ReadyHost {
        send: Option<Result<u64, isoplane_abi::native_io::Error>>,
    }
    impl HostBindings for ReadyHost {
        fn begin_slice(&mut self) {}
        fn end_slice(&mut self) {}
        fn slice_clock_ms(&self) -> u64 {
            1
        }
        fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
            0
        }
        fn call_sync(&mut self, _: ImportCall<'_>) -> i64 {
            0
        }
        fn native_io(
            &mut self,
            request: Request,
        ) -> Result<Response, isoplane_abi::native_io::Error> {
            if let Some(result) = &self.send {
                assert!(
                    matches!(request, Request::TrySend { ref pollables, ref bytes }
                    if pollables == &[ResourceHandle(7)] && bytes == b"abc"),
                    "the send bridge must preserve pollables and payload"
                );
                return result
                    .clone()
                    .map(|count| Response::Capacity(isoplane_abi::native_io::ByteCount(count)));
            }
            Ok(Response::Handle(ResourceHandle(1)))
        }
    }

    /// An address with at least `MAX_OUT_BUFFER_BYTES + 1` bytes after it that
    /// neither the packet nor the dispatch record occupies, so they read as
    /// zeros.
    const ZEROED: u32 = 65536;

    fn operation(symbol: &str) -> u32 {
        isoplane_abi::native_io::canonical::DEFINITIONS
            .iter()
            .find(|definition| definition.symbol == symbol)
            .unwrap_or_else(|| panic!("the canonical contract defines {symbol}"))
            .id
            .0
    }

    fn packet(operation: u32, pointer: u32, count: u32, setup: &str) -> Completion {
        capability_packet("poll_call", operation, pointer, count, setup)
    }

    fn capability_packet(
        capability: &str,
        operation: u32,
        pointer: u32,
        count: u32,
        setup: &str,
    ) -> Completion {
        let engine = test_engine();
        let wasm = guest(32,
        &format!(r#"(import "isoplane" "{capability}" (func $poll (param i32 i32 i32) (result i64)))"#),
        "", &format!("{setup} (drop (call $poll (i32.const {operation}) (i32.const {pointer}) (i32.const {count}))) {DONE}"), DONE);
        let template = engine
            .load_template(&wasm)
            .expect("the native readiness import must validate");
        let mut instance = engine
            .activate(&template, Box::new(ReadyHost::default()))
            .expect("the native readiness guest must activate");
        instance
            .dispatch(
                &DispatchRecord {
                    dispatch_id: 1,
                    kind: DispatchKind::Http(HttpRequest {
                        method: "GET".into(),
                        url: "https://readiness.test/".into(),
                        path: "/".into(),
                        query: vec![],
                        headers: vec![],
                    }),
                    cancel_future: 0,
                    occupancy_budget_ms: 30_000,
                    pending_futures_limit: 64,
                    now_ms: 1,
                },
                OccupancyBudget {
                    occupancy_ms: 30_000,
                    slice_wall_ms_cap: 60_000,
                    spent_ns: 0,
                },
            )
            .completion
    }

    fn send_operation() -> u32 {
        isoplane_abi::native_io::canonical::DEFINITIONS
            .iter()
            .find(|definition| {
                matches!(
                    definition.action,
                    isoplane_abi::native_io::canonical::Action::TrySendWithErrorCode
                )
            })
            .expect("the canonical contract defines the detailed send bridge")
            .id
            .0
    }

    fn send_bridge(answer: Result<u64, isoplane_abi::native_io::Error>) -> i64 {
        let engine = test_engine();
        let wasm = guest(2,
            r#"(import "isoplane" "socket_call" (func $socket (param i32 i32 i32) (result i64)))"#,
            r#"(data (i32.const 2000) "abc")"#,
            &format!("(i32.store (i32.const 3000) (i32.const 7)) (i64.store (i32.const 1100) (i64.const 3000)) (i64.store (i32.const 1108) (i64.const 1)) (i64.store (i32.const 1116) (i64.const 2000)) (i64.store (i32.const 1124) (i64.const 3)) (i64.store (i32.const 1000) (call $socket (i32.const {}) (i32.const 1100) (i32.const 4))) {DONE}", send_operation()), DONE);
        let template = engine
            .load_template(&wasm)
            .expect("the socket import must validate");
        let mut instance = engine
            .activate(&template, Box::new(ReadyHost { send: Some(answer) }))
            .expect("the send fixture must activate");
        assert_eq!(
            instance
                .dispatch(&super::http_dispatch(1), OccupancyBudget::default())
                .completion,
            Completion::Done,
            "a nonblocking send must return its result without trapping"
        );
        i64::from_le_bytes(
            instance.memory()[1000..1008]
                .try_into()
                .expect("the returned send result"),
        )
    }

    #[test]
    fn the_send_bridge_preserves_distinct_network_failures() {
        use isoplane_abi::native_io::network_error::NetworkErrorCode;
        for (kind, code) in [
            (
                std::io::ErrorKind::ConnectionReset,
                NetworkErrorCode::ConnectionReset,
            ),
            (
                std::io::ErrorKind::BrokenPipe,
                NetworkErrorCode::ConnectionReset,
            ),
            (
                std::io::ErrorKind::PermissionDenied,
                NetworkErrorCode::AccessDenied,
            ),
        ] {
            let error = isoplane_abi::native_io::Error::from(std::io::Error::from(kind));
            let encoded = send_bridge(Err(error)) as u64;
            assert_eq!(
                isoplane_abi::native_io::canonical::SEND_FAILED.checked_sub(encoded),
                Some(code as u64),
                "the send bridge must carry the actual network error for {kind:?}"
            );
        }
    }

    #[test]
    fn the_send_bridge_distinguishes_pending_from_partial_success() {
        assert_eq!(
            send_bridge(Err(isoplane_abi::native_io::Error::WouldBlock)),
            -1,
            "a send the socket cannot take yet must return the pending sentinel"
        );
        assert_eq!(
            send_bridge(Ok(2)),
            2,
            "a partial send must report the number of bytes actually accepted"
        );
    }

    #[test]
    fn readiness_permission_cannot_invoke_the_send_bridge() {
        assert_eq!(
            packet(send_operation(), 1000, 4, ""),
            Completion::Trapped,
            "a readiness-only packet must not acquire socket send authority"
        );
    }

    #[test]
    fn readiness_permission_cannot_create_a_network_resource() {
        let operation = isoplane_abi::native_io::canonical::DEFINITIONS
            .iter()
            .find(|definition| {
                matches!(
                    definition.action,
                    isoplane_abi::native_io::canonical::Action::Network
                )
            })
            .expect("the native contract must define network creation");
        assert_eq!(
            packet(operation.id.0, 1000, 0, ""),
            Completion::Trapped,
            "a guest with only readiness permission must not obtain network authority"
        );
    }

    #[test]
    fn a_native_send_cannot_allocate_an_unbounded_pollable_list() {
        let limit = isoplane_abi::imports::MAX_OUT_BUFFER_BYTES / 4;
        let send = |pollables: usize| {
            capability_packet(
                "socket_call",
                operation("isoplane_socket_try_send_with_error_code"),
                1000,
                4,
                &format!("(i64.store (i32.const 1008) (i64.const {pollables}))"),
            )
        };
        assert_eq!(
            send(limit),
            Completion::Done,
            "a native send list at the host buffer limit must reach the host"
        );
        assert_eq!(
            send(limit + 1),
            Completion::Trapped,
            "an oversized native send list must trap before host allocation"
        );
    }

    #[test]
    fn a_native_dns_name_cannot_exceed_the_host_buffer_budget() {
        let limit = isoplane_abi::imports::MAX_OUT_BUFFER_BYTES;
        let resolve = |length: usize| {
            capability_packet(
                "socket_call",
                operation("ip_name_lookup_resolve_addresses"),
                1000,
                4,
                &format!(
                    "(i64.store (i32.const 1008) (i64.const {ZEROED})) \
                     (i64.store (i32.const 1016) (i64.const {length}))"
                ),
            )
        };
        assert_eq!(
            resolve(limit),
            Completion::Done,
            "a DNS name at the host buffer limit must reach the resolver"
        );
        assert_eq!(
            resolve(limit + 1),
            Completion::Trapped,
            "an oversized DNS name must trap before host allocation"
        );
    }

    #[test]
    fn a_native_no_delay_packet_refuses_invalid_settings() {
        let no_delay = |setting: u64| {
            capability_packet(
                "socket_call",
                operation("isoplane_socket_no_delay"),
                1000,
                2,
                &format!("(i64.store (i32.const 1008) (i64.const {setting}))"),
            )
        };
        for setting in [0, 1, u64::from(u32::MAX)] {
            assert_eq!(
                no_delay(setting),
                Completion::Done,
                "TCP_NODELAY setting {setting} is false, true or a read"
            );
        }
        assert_eq!(
            no_delay(2),
            Completion::Trapped,
            "TCP_NODELAY accepts only a read, false, or true operation"
        );
    }

    #[test]
    fn a_native_dns_name_must_be_valid_utf8() {
        let resolve = |byte: u8| {
            capability_packet(
                "socket_call",
                operation("ip_name_lookup_resolve_addresses"),
                1000,
                4,
                &format!(
                    "(i64.store (i32.const 1008) (i64.const 2000)) \
                     (i64.store (i32.const 1016) (i64.const 1)) \
                     (i32.store8 (i32.const 2000) (i32.const {byte}))"
                ),
            )
        };
        assert_eq!(
            resolve(b'a'),
            Completion::Done,
            "a one-byte ASCII name must reach the resolver"
        );
        assert_eq!(
            resolve(0xff),
            Completion::Trapped,
            "invalid DNS name encoding must trap before resolver work starts"
        );
    }

    #[test]
    fn an_unknown_native_readiness_operation_is_rejected() {
        assert_eq!(
            packet(u32::MAX, 1000, 0, ""),
            Completion::Trapped,
            "unknown operation ids must never reach host resources"
        );
    }

    const TIMER: &str = "monotonic_clock_subscribe_duration";

    #[test]
    fn a_native_readiness_packet_must_match_its_canonical_argument_count() {
        assert_eq!(
            packet(operation(TIMER), 1000, 1, ""),
            Completion::Done,
            "a timer packet with its one argument must reach the host"
        );
        assert_eq!(
            packet(operation(TIMER), 1000, 2, ""),
            Completion::Trapped,
            "a timer packet must reject surplus arguments"
        );
    }

    #[test]
    fn native_readiness_packets_cannot_read_outside_guest_memory() {
        assert_eq!(
            packet(operation(TIMER), u32::MAX, 1, ""),
            Completion::Trapped,
            "an out-of-bounds timer packet must trap before reaching host resources"
        );
    }

    #[test]
    fn a_native_poll_list_cannot_exceed_the_host_buffer_budget() {
        let limit = isoplane_abi::imports::MAX_OUT_BUFFER_BYTES / 4;
        let poll = |pollables: usize| {
            packet(
                operation("poll_poll"),
                1000,
                3,
                &format!(
                    "(i64.store (i32.const 1000) (i64.const {ZEROED})) \
                     (i64.store (i32.const 1008) (i64.const {pollables}))"
                ),
            )
        };
        assert_eq!(
            poll(limit),
            Completion::Done,
            "a poll list at the host buffer limit must reach the host"
        );
        assert_eq!(
            poll(limit + 1),
            Completion::Trapped,
            "an oversized poll list must be rejected even when guest memory contains it"
        );
    }

    #[test]
    fn a_native_blocking_wait_is_refused_even_when_the_host_would_accept_it() {
        assert_eq!(
            packet(operation("poll_method_pollable_block"), 1000, 1, ""),
            Completion::Trapped,
            "a canonical blocking wait must never enter the host resource implementation"
        );
    }
}
