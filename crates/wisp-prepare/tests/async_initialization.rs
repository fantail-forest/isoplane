#![forbid(unsafe_code)]

mod common;

use wisp_abi::config::InitSandbox;
use wisp_abi::imports::Ty;
use wisp_abi::native_io::canonical::DEFINITIONS;
use wisp_prepare::{initialize, Initialization, InitializationNetwork, PrepareError};

fn request() -> Initialization {
    let mut request = common::request();
    request.sandbox = InitSandbox::RuntimeBringUp;
    request.network = Some(InitializationNetwork {
        tenant: "async-initialization-test".into(),
        allowlist: None,
        allowed_destinations: Vec::new(),
        limits: Default::default(),
        native_handles: 16,
        pending_futures: 4,
    });
    request
}

fn import(symbol: &str, name: &str) -> String {
    let definition = DEFINITIONS
        .iter()
        .find(|definition| definition.symbol == symbol)
        .unwrap();
    let signature = definition.pal.sig.unwrap();
    let ty = |ty: &Ty| match ty {
        Ty::I32 => "i32",
        Ty::I64 => "i64",
    };
    let parameters = signature
        .params
        .iter()
        .map(ty)
        .collect::<Vec<_>>()
        .join(" ");
    let results = signature
        .results
        .iter()
        .map(ty)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"(import "{}" "{}" (func ${name} (param {parameters}) (result {results})))"#,
        definition.pal.module, definition.pal.func
    )
}

fn asynchronous_guest(imports: &str, init: &str, wake: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module
      {imports}
      (memory (export "memory") 1 64)
      (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 0))
      (func (export "wisp_wake") (param i64 i32 i32) (result i64) {wake})
      (func (export "wisp_initialize") (result i64) {init}))"#
    ))
    .unwrap()
}

fn timer_guest() -> Vec<u8> {
    let imports = format!(
        "{} {}",
        import("monotonic_clock_subscribe_duration", "timer"),
        import("poll_pollable_drop", "drop_timer")
    );
    asynchronous_guest(&imports,
        "(i32.store (i32.const 64) (call $timer (i64.const 1000000))) (i64.const 1)",
        "(call $drop_timer (i32.load (i32.const 64))) (i32.store (i32.const 128) (i32.const 42)) (i64.const 0)")
}

#[cfg(target_os = "linux")]
#[test]
fn layered_preparation_captures_the_completed_async_initializer() {
    let engine = common::engine();
    let wasm = timer_guest();
    let build = wisp_prepare::BuildRequest {
        guest_memory_bytes: Some(wisp_abi::config::GuestMemoryBytes::new(4 << 20).unwrap()),
        validation_limits: Default::default(),
        application_files: Default::default(),
        engine: engine.configuration(),
        runtime: wisp_prepare::RuntimeProfile::raw_wasm(),
        limits: wisp_abi::config::Limits::default(),
        initialization: request(),
        input_hash: *blake3::hash(&wasm).as_bytes(),
    };
    let product = wisp_prepare::prepare_in_current_process(&wasm, &build).expect(
        "layered capture and fresh-instance state verification must succeed after the timer wake",
    );
    assert_eq!(
        product.initialized.passes, 1,
        "asynchronous completion must not rerun the initializer"
    );
    let portable = engine.load_template(&product.initialized.wasm).unwrap();
    assert_eq!(
        portable.memory_image_arc().to_dense()[128],
        42,
        "both captured outputs must contain initialization work performed after the timer wake"
    );
}

#[test]
fn initialization_resumes_after_host_timer_completion_before_capturing_memory() {
    let engine = common::engine();
    let wasm = timer_guest();
    let initialized = initialize(&engine, &wasm, &request()).unwrap();
    let template = engine.load_template(&initialized.wasm).unwrap();
    assert_eq!(
        template.memory_image_arc().to_dense()[128],
        42,
        "captured initialization memory must include work performed after the host wake"
    );
}

#[test]
fn a_pending_initializer_without_a_host_wake_source_is_refused() {
    let wasm = asynchronous_guest("", "(i64.const 1)", "(i64.const 0)");
    let result = initialize(&common::engine(), &wasm, &request());
    assert!(
        matches!(result, Err(PrepareError::InvalidRequest(_))),
        "pending initialization without host work must fail instead of capturing incomplete state"
    );
}

#[test]
fn a_completed_initializer_cannot_capture_a_retained_native_socket() {
    let wasm = common::guest(
        &import("tcp_create_socket_create_tcp_socket", "create"),
        "(call $create (i32.const 0) (i32.const 64))",
    );
    let result = initialize(&common::engine(), &wasm, &request());
    assert!(
        matches!(result, Err(PrepareError::State(_))),
        "returning Done must not authorize capturing an undisposed native socket"
    );
}

fn socket_imports() -> String {
    [
        ("instance_network_instance_network", "network"),
        ("tcp_create_socket_create_tcp_socket", "create"),
        ("tcp_method_tcp_socket_start_connect", "connect"),
        ("tcp_tcp_socket_drop", "drop_socket"),
    ]
    .iter()
    .map(|(symbol, name)| import(symbol, name))
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn preparation_reports_egress_denial_through_the_canonical_socket_interface() {
    let wasm = common::guest(
        &socket_imports(),
        r#"
      (i32.store (i32.const 32) (call $network))
      (call $create (i32.const 0) (i32.const 64))
      (call $connect (i32.load (i32.const 68)) (i32.load (i32.const 32))
        (i32.const 0) (i32.const 4321)
        (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1)
        (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
        (i32.const 128))
      (call $drop_socket (i32.load (i32.const 68)))"#,
    );
    let engine = common::engine();
    let initialized = initialize(&engine, &wasm, &request()).unwrap();
    let template = engine.load_template(&initialized.wasm).unwrap();
    let memory = template.memory_image_arc().to_dense();
    assert_eq!(
        memory[128], 1,
        "a private destination without an explicit allowance must return a canonical error"
    );
    assert_eq!(
        memory[129],
        wisp_abi::native_io::network_error::NetworkErrorCode::AccessDenied as u8,
        "the canonical error must identify the egress policy refusal"
    );
}

#[test]
fn an_allowed_initialization_connection_finishes_and_closes_before_capture() {
    use std::io::Read;

    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let destination = listener.local_addr().unwrap();
    let mut request = request();
    request
        .network
        .as_mut()
        .unwrap()
        .allowed_destinations
        .push(destination);
    let wasm = asynchronous_guest(
        &socket_imports(),
        &format!(
            r#"
      (i32.store (i32.const 32) (call $network))
      (call $create (i32.const 0) (i32.const 64))
      (call $connect (i32.load (i32.const 68)) (i32.load (i32.const 32))
        (i32.const 0) (i32.const {})
        (i32.const 127) (i32.const 0) (i32.const 0) (i32.const 1)
        (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
        (i32.const 128))
      (if (i32.load8_u (i32.const 128)) (then unreachable))
      (i64.const 1)"#,
            destination.port()
        ),
        r#"
      (call $drop_socket (i32.load (i32.const 68)))
      (i32.store (i32.const 192) (i32.const 77))
      (i64.const 0)"#,
    );
    let engine = common::engine();
    let initialized = initialize(&engine, &wasm, &request).unwrap();
    let template = engine.load_template(&initialized.wasm).unwrap();
    assert_eq!(template.memory_image_arc().to_dense()[192], 77, "initialization must resume after real TCP connection completion before capturing guest state");
    listener.set_nonblocking(true).unwrap();
    let (mut peer, _) = listener
        .accept()
        .expect("successful initialization must have reached the approved real endpoint");
    peer.set_nonblocking(false).unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    assert_eq!(
        peer.read(&mut [0; 1]).unwrap(),
        0,
        "all preparation connection ownership must be released before a template is returned"
    );
    let resources = wasmparser::Parser::new(0)
        .parse_all(&initialized.wasm)
        .find_map(|payload| match payload.unwrap() {
            wasmparser::Payload::CustomSection(section)
                if section.name() == wisp_abi::native_io::PREPARED_RESOURCES_SECTION =>
            {
                Some(
                    postcard::from_bytes::<wisp_abi::native_io::PreparedResources>(section.data())
                        .unwrap(),
                )
            }
            _ => None,
        })
        .expect("initialized output must carry native handle restoration state");
    assert_eq!(
        resources.network,
        Some(wisp_abi::native_io::ResourceHandle(1)),
        "the surviving guest network handle must retain its captured identity"
    );
    assert_eq!(
        resources.next_handle, 3,
        "activation must not reuse the disposed initialization socket handle"
    );
}
