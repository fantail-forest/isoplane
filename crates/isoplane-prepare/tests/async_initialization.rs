#![cfg_attr(not(target_os = "linux"), forbid(unsafe_code))]

mod common;
#[path = "common/packets.rs"]
mod packets;

use isoplane_abi::imports::Ty;
use isoplane_abi::native_io::canonical::DEFINITIONS;
use isoplane_harness::preparation::{self, staged_guest, Prepared};
use isoplane_prepare::{Initialization, InitializationNetwork, PrepareError};

/// Prepares `wasm` the way a node prepares an application on a shared runtime.
fn prepare(wasm: &[u8], initialization: Initialization) -> Result<Prepared, PrepareError> {
    preparation::prepare(&preparation::request(
        wasm,
        common::engine_config(),
        initialization,
    ))
}

/// What preparing `wasm`'s application stage refused, its runtime stage
/// having prepared as a baseline.
fn refusal(wasm: &[u8], initialization: Initialization) -> PrepareError {
    preparation::application_refusal(&preparation::request(
        wasm,
        common::engine_config(),
        initialization,
    ))
}

fn request() -> Initialization {
    let mut request = common::request();
    request.network = Some(InitializationNetwork {
        tenant: "async-initialization-test".into(),
        allowlist: None,
        destinations: isoplane_prepare::InitializationDestinations::Public {
            exceptions: Vec::new(),
        },
        limits: Default::default(),
        native_handles: 16,
        open_connections: isoplane_abi::config::DEFAULT_INSTANCE_OPEN_CONNECTIONS,
        pending_futures: 4,
        buffered_bytes: 1024,
    });
    request
}

/// The WASI import the shared runtime declares for the operation `symbol`.
fn wasi_import(symbol: &str, name: &str) -> String {
    let import = isoplane_abi::pal::PAL_IMPORTS
        .iter()
        .find(|import| import.operation.symbol == symbol)
        .unwrap();
    let ty = |ty: &Ty| match ty {
        Ty::I32 => "i32",
        Ty::I64 => "i64",
    };
    let signature = import.sig();
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
        import.module, import.func
    )
}

/// The socket import and a function `$name` for each socket operation `symbol`.
fn socket_functions(operations: &[(&str, &str)]) -> String {
    let functions = operations
        .iter()
        .map(|(symbol, name)| {
            let definition = DEFINITIONS
                .iter()
                .find(|definition| definition.symbol == *symbol)
                .unwrap();
            packets::function(name, "socket_call", definition.id.0, definition.sig)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\n{functions}", packets::SOCKET_IMPORT)
}

fn asynchronous_guest(imports: &str, init: &str, wake: &str) -> Vec<u8> {
    wat::parse_str(format!(
        r#"(module
      {imports}
      (memory (export "memory") 1 64)
      (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 2048))
      (func (export "isoplane_handle") (param i32 i32) (result i64) (i64.const 0))
      (func (export "isoplane_wake") (param i64 i32 i32) (result i64) {wake})
      (func (export "isoplane_initialize_runtime") (result i64) (i64.const 0))
      (func (export "isoplane_initialize") (result i64) {init}))"#
    ))
    .unwrap()
}

fn timer_guest() -> Vec<u8> {
    let imports = format!(
        "{} {}",
        wasi_import("monotonic_clock_subscribe_duration", "timer"),
        wasi_import("poll_pollable_drop", "drop_timer")
    );
    asynchronous_guest(&imports,
        "(i32.store (i32.const 64) (call $timer (i64.const 1000000))) (i64.const 1)",
        "(call $drop_timer (i32.load (i32.const 64))) (i32.store (i32.const 128) (i32.const 42)) (i64.const 0)")
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn layered_preparation_captures_the_completed_async_initializer() {
    let engine = common::engine();
    let wasm = timer_guest();
    let build = isoplane_prepare::BuildRequest {
        guest_memory_bytes: Some(isoplane_abi::config::GuestMemoryBytes::new(4 << 20).unwrap()),
        validation_limits: Default::default(),
        application_files: Default::default(),
        engine: engine.configuration(),
        runtime: isoplane_prepare::RuntimeProfile::RawWasm,
        memory_limit: isoplane_abi::config::GuestMemoryBytes::default(),
        initialization: request(),
        module: isoplane_prepare::Module::new(&wasm),
    };
    use isoplane_prepare::Preparation;
    let product = isoplane_harness::preparation::local_preparation()
        .prepare(&build, &tokio_util::sync::CancellationToken::new()).await.expect(
        "layered capture and fresh-instance state verification must succeed after the timer wake",
    );
    // SAFETY: the local preparation provider above produced this native code.
    let artifact = unsafe {
        engine.decode_builder_output(product.prepared_bytes(), build.identity().unwrap())
    }
    .unwrap();
    let prepared = engine
        .load_prepared_product_in(&artifact, &std::env::temp_dir())
        .unwrap();
    assert_eq!(
        prepared.memory_image_arc().to_dense()[128],
        42,
        "prepared state must contain initialization work performed after the timer wake"
    );
}

#[test]
fn initialization_resumes_after_host_timer_completion_before_capturing_memory() {
    let prepared = prepare(&timer_guest(), request()).unwrap();
    assert_eq!(
        prepared.memory()[128],
        42,
        "captured initialization memory must include work performed after the host wake"
    );
}

#[test]
fn a_pending_initializer_without_a_host_wake_source_is_refused() {
    let wasm = asynchronous_guest("", "(i64.const 1)", "(i64.const 0)");
    let error = refusal(&wasm, request());
    assert!(
        matches!(error, PrepareError::InvalidRequest(_)),
        "pending initialization without host work must fail instead of capturing incomplete state: {error}"
    );
}

#[test]
fn a_refused_capture_carries_the_guests_warnings_without_its_routine_logs() {
    let wasm = staged_guest(
        &format!(
            r#"(import "isoplane" "log_emit" (func $log (param i32 i32 i32))) {}
            (data (i32.const 128) "entering the initializer")
            (data (i32.const 192) "the cache client is still connected")"#,
            socket_functions(&[("tcp_create_socket_create_tcp_socket", "create")])
        ),
        "",
        r#"(call $log (i32.const 1) (i32.const 128) (i32.const 24))
        (call $log (i32.const 3) (i32.const 192) (i32.const 35))
        (call $create (i32.const 0) (i32.const 64))"#,
    );
    let error = refusal(&wasm, request());
    assert!(
        matches!(error, PrepareError::State(_)),
        "a retained socket must refuse the capture: {error}"
    );
    let detail = error.to_string();
    assert!(
        detail.contains("the cache client is still connected"),
        "the refusal must carry what the guest warned of: {detail}"
    );
    assert!(
        !detail.contains("entering the initializer"),
        "a guest that returned successfully must not bury its warnings in routine logs: {detail}"
    );
}

fn socket_imports() -> String {
    socket_functions(&[
        ("instance_network_instance_network", "network"),
        ("tcp_create_socket_create_tcp_socket", "create"),
        ("tcp_method_tcp_socket_start_connect", "connect"),
        ("tcp_tcp_socket_drop", "drop_socket"),
    ])
}

#[test]
fn preparation_reports_egress_denial_through_the_canonical_socket_interface() {
    let wasm = staged_guest(
        &socket_imports(),
        "",
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
    let memory = prepare(&wasm, request()).unwrap().memory();
    assert_eq!(
        memory[128], 1,
        "a private destination without an explicit allowance must return a canonical error"
    );
    assert_eq!(
        memory[129],
        isoplane_abi::native_io::network_error::NetworkErrorCode::AccessDenied as u8,
        "the canonical error must identify the egress policy refusal"
    );
}

#[test]
fn an_allowed_initialization_connection_finishes_and_closes_before_capture() {
    use std::io::Read;

    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let destination = listener.local_addr().unwrap();
    let mut request = request();
    request.network.as_mut().unwrap().destinations =
        isoplane_prepare::InitializationDestinations::Public {
            exceptions: vec![destination],
        };
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
    let prepared = prepare(&wasm, request).unwrap();
    assert_eq!(
        prepared.memory()[192],
        77,
        "initialization must resume after real TCP connection completion before capturing guest state"
    );
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
    let restored = std::sync::Arc::new(parking_lot::Mutex::new(None));
    prepared
        .engine
        .activate(
            &prepared.template,
            Box::new(RestoredResources(restored.clone())),
        )
        .expect("the template must activate");
    let resources = restored
        .lock()
        .take()
        .expect("activation must restore the template's native handle state");
    assert_eq!(
        resources.network,
        Some(isoplane_abi::native_io::ResourceHandle(1)),
        "the surviving guest network handle must retain its captured identity"
    );
    assert_eq!(
        resources.next_handle, 3,
        "activation must not reuse the disposed initialization socket handle"
    );
}

/// Records the native resource state an activation restores from its template.
struct RestoredResources(
    std::sync::Arc<parking_lot::Mutex<Option<isoplane_abi::native_io::PreparedResources>>>,
);

impl isoplane_engine::bindings::HostBindings for RestoredResources {
    fn restore_prepared_resources(
        &mut self,
        resources: &isoplane_abi::native_io::PreparedResources,
    ) -> Result<(), isoplane_engine::bindings::BindingError> {
        *self.0.lock() = Some(resources.clone());
        Ok(())
    }
    fn call_future(&mut self, _call: isoplane_engine::ImportCall<'_>) -> u64 {
        0
    }
    fn call_sync(&mut self, _call: isoplane_engine::ImportCall<'_>) -> i64 {
        0
    }
    fn slice_clock_ms(&self) -> u64 {
        0
    }
    fn begin_slice(&mut self) {}
    fn end_slice(&mut self) {}
}
