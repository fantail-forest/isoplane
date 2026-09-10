//! The WASI PAL adapter exercised through the full engine: composition,
//! the quantized frozen clock, entropy delegation, and trap-on-call stubs.

mod common;

use common::*;
use wisp_engine::{Completion, CpuBudget, InstanceOps};

fn read_u32(m: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(m[off..off + 4].try_into().expect("4 bytes"))
}

/// A `DispatchRecord` with nothing interesting in it.
fn http_dispatch(id: u64) -> wisp_abi::dispatch::DispatchRecord {
    use wisp_abi::dispatch::*;
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
        cpu_budget_ms: 30_000,
        now_ms: 1_700_000_000_000,
    }
}

/// A guest that imports `wasi:*` as well as `wisp`, with the `cabi_realloc`
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
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
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

    let read = |i: &mut wisp_engine::Instance, off: usize| -> i64 {
        let m = i.memory();
        i64::from_le_bytes(m[off..off + 8].try_into().expect("8 bytes"))
    };

    assert_eq!(
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
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
        i.dispatch(&http_dispatch(2), CpuBudget::http()).completion,
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
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
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

    let out = i.dispatch(&http_dispatch(1), CpuBudget::http());
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
        i.reset().is_err(),
        "a trapped instance is discarded, not reset"
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
        i.dispatch(&http_dispatch(1), CpuBudget::http()).completion,
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
    use wisp_abi::dispatch::{DispatchKind, DispatchRecord, HttpRequest, TraceContext};
    use wisp_abi::native_io::{Request, ResourceHandle, Response};
    use wisp_engine::{Completion, CpuBudget, HostBindings, ImportCall, InstanceOps};

    struct ReadyHost;
    impl HostBindings for ReadyHost {
        fn slice_clock_ms(&self) -> u64 {
            1
        }
        fn call_future(&mut self, _: ImportCall<'_>) -> u64 {
            0
        }
        fn call_sync(&mut self, _: ImportCall<'_>) -> i64 {
            0
        }
        fn native_io(&mut self, request: Request) -> Result<Response, wisp_abi::native_io::Error> {
            Ok(match request {
                Request::Http(_) => Response::Http(wisp_abi::native_io::http::Response::Handle(
                    ResourceHandle(1),
                )),
                _ => Response::Handle(ResourceHandle(1)),
            })
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
        &format!(r#"(import "wisp" "{capability}" (func $poll (param i32 i32 i32) (result i64)))"#),
        "", &format!("{setup} (drop (call $poll (i32.const {operation}) (i32.const {pointer}) (i32.const {count}))) {DONE}"), DONE);
        let template = engine
            .load_template(&wasm)
            .expect("the native readiness import must validate");
        let mut instance = engine
            .activate(&template, Box::new(ReadyHost))
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
                    cpu_budget_ms: 30_000,
                    now_ms: 1,
                },
                CpuBudget {
                    cpu_ms: 30_000,
                    wall_ms_cap: 60_000,
                    spent_ns: 0,
                },
            )
            .completion
    }

    #[test]
    fn readiness_permission_cannot_create_a_network_resource() {
        let operation = wisp_abi::native_io::canonical::DEFINITIONS
            .iter()
            .find(|definition| {
                matches!(
                    definition.action,
                    wisp_abi::native_io::canonical::Action::Network
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
        let count = wisp_abi::imports::MAX_OUT_BUFFER_BYTES / 4 + 1;
        let setup = format!("(i64.store (i32.const 1008) (i64.const {count}))");
        assert_eq!(
            capability_packet("socket_call", 69, 1000, 4, &setup),
            Completion::Trapped,
            "an oversized native send list must trap before host allocation"
        );
    }

    #[test]
    fn a_native_dns_name_cannot_exceed_the_host_buffer_budget() {
        let count = wisp_abi::imports::MAX_OUT_BUFFER_BYTES + 1;
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
        let count = wisp_abi::imports::MAX_OUT_BUFFER_BYTES / 4 + 1;
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

    /// The id of the canonical entry named `function` in `module`.
    fn operation(module: &str, function: &str) -> u32 {
        wisp_abi::native_io::canonical::DEFINITIONS
            .iter()
            .find(|definition| definition.pal.module == module && definition.pal.func == function)
            .unwrap_or_else(|| panic!("the canonical table must define {module}::{function}"))
            .id
            .0
    }

    const HTTP_TYPES: &str = "wasi:http/types@0.2.0";

    #[test]
    fn an_http_packet_with_its_canonical_argument_count_reaches_the_host() {
        assert_eq!(
            capability_packet(
                "http_call",
                operation(HTTP_TYPES, "[constructor]fields"),
                1000,
                0,
                ""
            ),
            Completion::Done,
            "a field set constructor packet must reach the host and answer with its handle"
        );
    }

    #[test]
    fn http_permission_cannot_create_a_socket_and_socket_permission_cannot_build_a_request() {
        assert_eq!(
            capability_packet(
                "http_call",
                operation("wasi:sockets/tcp-create-socket@0.2.0", "create-tcp-socket"),
                1000,
                2,
                ""
            ),
            Completion::Trapped,
            "a guest with only HTTP permission must not obtain a socket"
        );
        assert_eq!(
            capability_packet(
                "socket_call",
                operation(HTTP_TYPES, "[constructor]outgoing-request"),
                1000,
                1,
                ""
            ),
            Completion::Trapped,
            "a guest with only socket permission must not build an HTTP request"
        );
    }

    #[test]
    fn an_http_field_list_cannot_exceed_the_host_buffer_budget() {
        let count = wisp_abi::imports::MAX_OUT_BUFFER_BYTES / 16 + 1;
        let setup = format!("(i64.store (i32.const 1000) (i64.const 2000)) (i64.store (i32.const 1008) (i64.const {count}))");
        assert_eq!(
            capability_packet(
                "http_call",
                operation(HTTP_TYPES, "[static]fields.from-list"),
                1000,
                3,
                &setup
            ),
            Completion::Trapped,
            "an oversized field list must trap before host allocation"
        );
    }

    #[test]
    fn an_http_option_discriminant_outside_the_canonical_form_is_rejected() {
        // option<string> takes 0 or 1; 2 is not a canonical option.
        let setup = "(i64.store (i32.const 1008) (i64.const 2))";
        assert_eq!(
            capability_packet(
                "http_call",
                operation(HTTP_TYPES, "[method]outgoing-request.set-path-with-query"),
                1000,
                4,
                setup
            ),
            Completion::Trapped,
            "an invalid option discriminant must trap before reaching host resources"
        );
    }

    #[test]
    fn the_request_options_the_client_never_sets_trap_on_call() {
        assert_eq!(
            capability_packet(
                "http_call",
                operation(HTTP_TYPES, "[constructor]request-options"),
                1000,
                0,
                ""
            ),
            Completion::Trapped,
            "request options are not implemented and must never reach host resources"
        );
    }
}
