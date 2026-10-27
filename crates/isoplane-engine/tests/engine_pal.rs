//! The WASI PAL adapter exercised through the full engine: composition,
//! the quantized frozen clock, entropy delegation, and trap-on-call stubs.

mod common;

use common::*;
use isoplane_engine::{Completion, InstanceOps, OccupancyBudget};

fn read_u32(m: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(m[off..off + 4].try_into().expect("4 bytes"))
}

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
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        occupancy_budget_ms: 30_000,
        pending_futures_limit: 64,
        now_ms: 1_700_000_000_000,
    }
}

/// A guest that imports `wasi:*` as well as `isoplane`, with the `cabi_realloc`
/// export the canonical ABI needs to return a `list<u8>`.
fn pal_guest(handle: &str) -> Vec<u8> {
    let imports = String::from(
        r#"
  (import "wasi:clocks/monotonic-clock@0.2.0" "now" (func $wasi_now (result i64)))
  (import "wasi:random/random@0.2.0" "get-random-bytes" (func $wasi_random (param i64 i32)))
  (import "wasi:cli/exit@0.2.0" "exit" (func $wasi_exit (param i32)))
  (import "wasi:cli/environment@0.2.0" "get-environment" (func $wasi_env (param i32)))
"#,
    );
    let funcs = r#"
  (global $cabi (mut i32) (i32.const 40000))
  (func (export "cabi_realloc")
        (param $old i32) (param $oldsz i32) (param $align i32) (param $newsz i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $cabi))
    (global.set $cabi
      (i32.and (i32.add (i32.add (local.get $p) (local.get $newsz)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))
"#;
    guest(4, &imports, funcs, handle, DONE)
}

#[test]
fn a_module_with_wasi_imports_instantiates_through_the_pal_adapter() {
    let e = test_engine();
    let wasm = pal_guest("(i64.const 0)");
    let t = e
        .load_template(&wasm)
        .expect("a whitelisted wasi: surface must instantiate");

    assert!(t.uses_pal(), "the adapter was composed for this module");
    let composed: Vec<String> = t.pal_composed().iter().map(|d| d.full_name()).collect();
    assert_eq!(
        composed.len(),
        4,
        "every declared wasi: import was composed: {composed:?}"
    );

    let mut caps: Vec<&str> = t
        .pal_composed()
        .iter()
        .filter_map(|d| d.disposition.capability())
        .collect();
    caps.sort_unstable();
    assert_eq!(
        caps,
        vec!["clock_now_coarse", "entropy_fill"],
        "the adapter drew on exactly the two declared capabilities"
    );

    let mut i = activate(&e, &t);
    assert_eq!(
        i.dispatch(&http_dispatch(1), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "the module dispatches"
    );
}

/// A nanosecond monotonic clock would pass "the guest can read the time" and
/// fail both assertions; a fine timer is the measurement primitive
/// Spectre-class attacks need.
#[test]
fn the_pal_clock_is_quantized_and_frozen_within_a_slice() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (i64.store (i32.const 3000) (call $wasi_now))
    (i64.store (i32.const 3008) (call $wasi_now))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    let read = |i: &mut isoplane_engine::Instance, off: usize| -> i64 {
        let m = i.memory();
        i64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
    };

    assert_eq!(
        i.dispatch(&http_dispatch(1), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "the dispatch completes"
    );
    let (a, b) = (read(&mut i, 3000), read(&mut i, 3008));
    assert_eq!(
        a, b,
        "the clock is frozen within a slice, so two reads cannot differ"
    );
    assert_eq!(
        a % 1_000_000,
        0,
        "the delegated clock is quantized to the coarse clock's millisecond: {a} ns"
    );

    assert_eq!(
        i.dispatch(&http_dispatch(2), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "the second dispatch completes"
    );
    let c = read(&mut i, 3000);
    assert!(
        c > a,
        "the coarse clock must advance between slices ({a} then {c}), or the freeze test is \
         vacuous"
    );
    assert_eq!(
        c % 1_000_000,
        0,
        "quantization must survive the slice boundary"
    );
}

/// Entropy captured into a template, or memoised anywhere on the delegation
/// path, would be shared by every clone and look fine, because random-looking
/// bytes arrive either way.
#[test]
fn the_pal_entropy_delegation_draws_per_call_and_never_caches() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (call $wasi_random (i64.const 16) (i32.const 3000))
    (call $wasi_random (i64.const 16) (i32.const 3016))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    assert_eq!(
        i.dispatch(&http_dispatch(1), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "the dispatch completes"
    );

    let (p1, l1, p2, l2) = {
        let m = i.memory();
        (
            read_u32(m, 3000) as usize,
            read_u32(m, 3004),
            read_u32(m, 3016) as usize,
            read_u32(m, 3020),
        )
    };
    assert_eq!(
        (l1, l2),
        (16, 16),
        "the adapter returns the requested length"
    );
    assert_ne!(
        p1, p2,
        "each draw allocates its own buffer through `cabi_realloc`"
    );

    let (first, second) = {
        let m = i.memory();
        (m[p1..p1 + 16].to_vec(), m[p2..p2 + 16].to_vec())
    };
    assert_ne!(
        first, second,
        "two draws in one dispatch returned identical bytes, so something on the delegation \
         path cached the CSPRNG output"
    );
}

/// A stub that returned zero would let the guest carry on, turning a denied
/// capability into a supported one that answers badly.
#[test]
fn a_pal_trap_on_call_stub_traps_rather_than_answering() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (call $wasi_exit (i32.const 0))
    (i64.store (i32.const 3000) (i64.const 1))   ;; must never run
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);

    let out = i.dispatch(&http_dispatch(1), OccupancyBudget::default());
    assert_eq!(
        out.completion,
        Completion::Trapped,
        "a denied capability must not answer"
    );
    let msg = out.trap.map(|t| t.message).unwrap_or_default();
    assert!(
        msg.contains("wasi:cli/exit") && msg.contains("trap-on-call"),
        "the trap must name the import and say why: {msg}"
    );
    assert!(
        i.capture_mutable_state().is_err(),
        "a trapped instance is discarded"
    );
}

#[test]
fn the_pal_environment_delegation_answers_empty() {
    let e = test_engine();
    let wasm = pal_guest(
        r#"
    (i64.store (i32.const 3000) (i64.const -1))  ;; poison the return area
    (call $wasi_env (i32.const 3000))
    (i64.const 0)"#,
    );
    let t = e.load_template(&wasm).expect("template");
    let mut i = activate(&e, &t);
    assert_eq!(
        i.dispatch(&http_dispatch(1), OccupancyBudget::default())
            .completion,
        Completion::Done,
        "the dispatch completes"
    );

    let (ptr, len) = {
        let m = i.memory();
        (read_u32(m, 3000), read_u32(m, 3004))
    };
    assert_eq!(
        (ptr, len),
        (0, 0),
        "there is no ambient environment, so the list is empty"
    );
}

mod native_readiness {
    use super::common::{guest, test_engine, DONE};
    use isoplane_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
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
                        body: vec![],
                    }),
                    trace: TraceContext::default(),
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

    fn send_bridge(
        import: &str,
        answer: Result<u64, isoplane_abi::native_io::Error>,
        legacy_wrapper: bool,
    ) -> i64 {
        let engine = test_engine();
        // The v1 C wrapper recognizes only -1 (pending) and -2 (failed),
        // returning -1 for either; any other value becomes ssize_t directly.
        let decode = if legacy_wrapper {
            "(if (result i64) (i32.or (i64.eq (local.get $result) (i64.const -1)) \
                (i64.eq (local.get $result) (i64.const -2))) \
                (then (i64.const -1)) (else (local.get $result)))"
        } else {
            "(local.get $result)"
        };
        let wasm = guest(2,
            &format!(r#"(import "isoplane:native-sockets" "{import}" (func $send (param i32 i32 i32 i32) (result i64)))"#),
            &format!(r#"(data (i32.const 2000) "abc")
                (func $wrapper (result i64) (local $result i64)
                    (local.set $result (call $send (i32.const 3000) (i32.const 1) (i32.const 2000) (i32.const 3)))
                    {decode})"#),
            &format!("(i32.store (i32.const 3000) (i32.const 7)) (i64.store (i32.const 1000) (call $wrapper)) {DONE}"), DONE);
        let template = engine
            .load_template(&wasm)
            .expect("the socket bridge must remain admitted");
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
    fn legacy_send_errors_still_reach_the_original_wrappers_failure_branch() {
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::PermissionDenied,
        ] {
            let error = isoplane_abi::native_io::Error::from(std::io::Error::from(kind));
            assert_eq!(send_bridge("try-send", Err(error), true), -1,
                "an existing guest must not receive an unrecognized negative byte count for {kind:?}");
        }
    }

    #[test]
    fn the_versioned_send_bridge_preserves_distinct_network_failures() {
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
            let encoded = send_bridge("try-send-with-error-code", Err(error), false) as u64;
            assert_eq!(
                isoplane_abi::native_io::canonical::send_failure_code(encoded),
                Some(code as u8),
                "the new bridge must carry the actual network error for {kind:?}"
            );
        }
    }

    #[test]
    fn both_send_bridges_distinguish_pending_from_partial_success() {
        for import in ["try-send", "try-send-with-error-code"] {
            assert_eq!(
                send_bridge(
                    import,
                    Err(isoplane_abi::native_io::Error::WouldBlock),
                    false
                ),
                -1,
                "{import} must retain the pending sentinel"
            );
            assert_eq!(
                send_bridge(import, Ok(2), false),
                2,
                "{import} must report the number of bytes actually accepted"
            );
        }
    }

    #[test]
    fn readiness_permission_cannot_invoke_the_versioned_send_bridge() {
        let operation = isoplane_abi::native_io::canonical::DEFINITIONS
            .iter()
            .find(|definition| definition.pal.func == "try-send-with-error-code")
            .expect("the canonical contract defines the detailed send bridge");
        assert_eq!(
            packet(operation.id.0, 1000, 4, ""),
            Completion::Trapped,
            "a readiness-only packet must not acquire socket send authority"
        );
    }

    #[test]
    fn the_versioned_send_bridge_rejects_a_wrong_import_signature() {
        let wasm = guest(
            2,
            r#"(import "isoplane:native-sockets" "try-send-with-error-code" (func $send (param i32) (result i64)))"#,
            "",
            DONE,
            DONE,
        );
        assert!(
            test_engine().load_template(&wasm).is_err(),
            "the detailed send bridge must enforce its canonical signature"
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
    fn readiness_permission_cannot_invoke_clock_capabilities() {
        for symbol in ["monotonic_clock_now", "monotonic_clock_resolution"] {
            let operation = isoplane_abi::native_io::canonical::DEFINITIONS
                .iter()
                .find(|definition| definition.symbol == symbol)
                .expect("the native contract must define the clock operation");
            assert_eq!(
                packet(operation.id.0, 1000, 0, ""),
                Completion::Trapped,
                "{symbol} must require the clock capability rather than readiness permission"
            );
        }
    }

    #[test]
    fn a_native_send_cannot_allocate_an_unbounded_pollable_list() {
        let count = isoplane_abi::imports::MAX_OUT_BUFFER_BYTES / 4 + 1;
        let setup = format!("(i64.store (i32.const 1008) (i64.const {count}))");
        assert_eq!(
            capability_packet("socket_call", 69, 1000, 4, &setup),
            Completion::Trapped,
            "an oversized native send list must trap before host allocation"
        );
    }

    #[test]
    fn a_native_dns_name_cannot_exceed_the_host_buffer_budget() {
        let count = isoplane_abi::imports::MAX_OUT_BUFFER_BYTES + 1;
        let setup = format!("(i64.store (i32.const 1016) (i64.const {count}))");
        assert_eq!(
            capability_packet("socket_call", 70, 1000, 4, &setup),
            Completion::Trapped,
            "an oversized DNS name must trap before host allocation"
        );
    }

    #[test]
    fn a_native_no_delay_packet_refuses_invalid_settings() {
        assert_eq!(
            capability_packet(
                "socket_call",
                74,
                1000,
                2,
                "(i64.store (i32.const 1008) (i64.const 2))"
            ),
            Completion::Trapped,
            "TCP_NODELAY accepts only a read, false, or true operation"
        );
    }

    #[test]
    fn a_native_dns_name_must_be_valid_utf8() {
        let setup = "(i64.store (i32.const 1008) (i64.const 2000)) (i64.store (i32.const 1016) (i64.const 1)) (i32.store8 (i32.const 2000) (i32.const 255))";
        assert_eq!(
            capability_packet("socket_call", 70, 1000, 4, setup),
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

    #[test]
    fn a_native_readiness_packet_must_match_its_canonical_argument_count() {
        assert_eq!(
            packet(0, 1000, 2, ""),
            Completion::Trapped,
            "a timer packet must reject surplus arguments"
        );
        assert_eq!(
            packet(0, 1000, 1, ""),
            Completion::Done,
            "a timer packet with its canonical argument count must reach the host"
        );
    }

    #[test]
    fn native_readiness_packets_cannot_read_outside_guest_memory() {
        assert_eq!(
            packet(0, u32::MAX, 1, ""),
            Completion::Trapped,
            "an out-of-bounds timer packet must trap before reaching host resources"
        );
    }

    #[test]
    fn a_native_poll_list_cannot_exceed_the_host_buffer_budget() {
        let count = isoplane_abi::imports::MAX_OUT_BUFFER_BYTES / 4 + 1;
        let setup = format!("(i64.store (i32.const 1000) (i64.const 2000)) (i64.store (i32.const 1008) (i64.const {count}))");
        assert_eq!(
            packet(2, 1000, 3, &setup),
            Completion::Trapped,
            "an oversized poll list must be rejected even when guest memory contains it"
        );
    }

    #[test]
    fn a_native_blocking_wait_is_refused_even_when_the_host_would_accept_it() {
        assert_eq!(
            packet(59, 1000, 1, ""),
            Completion::Trapped,
            "a canonical blocking wait must never enter the host resource implementation"
        );
    }
}
