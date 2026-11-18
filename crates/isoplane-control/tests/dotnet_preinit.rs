//! The shipped .NET guest through pre-initialization: the template answers
//! its first call with the managed runtime already up and no init entry left
//! to run.

use isoplane_abi::config::GuestMemoryBytes;
use isoplane_control::validator;
#[path = "common/managed.rs"]
mod managed;

#[tokio::test]
async fn the_dotnet_template_activates_without_running_the_runtime_start_up() {
    let wasm = isoplane_fixtures::mono_runtime();
    validator::validate_with_memory_limit(
        &wasm,
        isoplane_abi::config::GuestMemoryBytes::default(),
        Some(GuestMemoryBytes::WASM32_MAX),
    )
    .expect(".NET guest must validate");

    let out = managed::prepare("isoplane_guest_dotnet_bcl", Default::default())
        .expect("pre-init must succeed on the managed application");
    assert!(
        !out.product.initialized.pal_calls.is_empty(),
        "the runtime reaches the PAL while coming up; nothing reached means no runtime start-up \
         was captured"
    );

    let artifact = out.artifact();
    let engine = isoplane_engine::IsoplaneEngine::new(isoplane_fixtures::engine_config(2)).unwrap();
    let template = engine
        .load_prepared_product(&artifact)
        .expect("prepared template");
    assert!(
        !template.has_initialize(),
        "prepared activation must have no initializer to rerun"
    );
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId(1));
    let mut guest = isoplane_harness::ReturnedGuest::prepared(artifact, &reactor);
    assert_eq!(guest.answer(&reactor, "/trust/at-boot", 4).await, "roots=0 ",
        "the first dispatch must observe application state already captured with an empty trust bundle");
}

#[test]
fn a_failed_managed_application_initializer_refuses_pre_initialization() {
    let result = managed::prepare("isoplane_guest_dotnet_failing_boot", Default::default());
    let Err(error) = result else {
        panic!("a failed application initializer must fail deployment");
    };
    assert_eq!(
        error.stage,
        isoplane_prepare::BuildStage::Initialization,
        "the deployment must fail while initializing the application"
    );
}

/// What a fixture leaves open, as the detail of the refused preparation.
fn refusal(
    runtime_config: &str,
    network: Option<isoplane_prepare::InitializationNetwork>,
) -> String {
    let result = managed::prepare_configured(
        "isoplane_guest_dotnet_failing_boot",
        Default::default(),
        runtime_config.as_bytes(),
        network,
    );
    let Err(error) = result else {
        panic!("a connection left open by initialization must refuse the template");
    };
    assert_eq!(
        error.stage,
        isoplane_prepare::BuildStage::Initialization,
        "the refusal must come from initialization"
    );
    error.detail
}

fn local_network() -> isoplane_prepare::InitializationNetwork {
    let host = isoplane_host::CellConfig::default();
    isoplane_prepare::InitializationNetwork {
        tenant: "fixture-tenant".into(),
        allowlist: None,
        destinations: isoplane_prepare::InitializationDestinations::LocalDevelopment,
        limits: Default::default(),
        native_handles: host.guest_resources.native_handles,
        open_connections: 8,
        pending_futures: 64,
        buffered_bytes: host.buffered_future_bytes,
    }
}

#[test]
fn a_socket_left_open_by_initialization_is_reported_with_where_it_was_opened() {
    let detail = refusal(r#"{"Failure":"open-socket"}"#, Some(local_network()));
    assert!(
        detail.contains("initialization retains a TCP socket")
            && detail.contains("a Tcp socket that is not connected, opened")
            && detail.contains("FailingBoot")
            && detail.contains("InitializeAsync"),
        "the refusal must give the host's reason, the socket and the method that opened it: {detail}"
    );
    assert_eq!(
        detail.matches("initialization left open").count(),
        1,
        "the refusal must report the open socket once: {detail}"
    );
}

#[test]
fn a_pooled_http_connection_left_open_is_reported_with_where_its_handler_was_created() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local listener");
    let endpoint = listener.local_addr().expect("the listener's address");
    let (release, held) = std::sync::mpsc::channel::<()>();
    // Answers one request and keeps the connection open, so the client pools it.
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the guest connects");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).expect("the request arrives");
            assert!(read > 0, "the guest must send a complete request");
            request.extend_from_slice(&chunk[..read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("the response is sent");
        held.recv().ok();
    });
    let detail = refusal(
        &format!(r#"{{"Failure":"pooled-http-connection","Endpoint":"http://{endpoint}/"}}"#),
        Some(local_network()),
    );
    drop(release);
    assert!(
        detail.contains("initialization retains a TCP socket")
            && detail.contains(&format!("a Tcp socket connected to {endpoint}"))
            && detail.contains("in the connection pool of an HTTP handler created")
            && detail.contains("System.Net.Http.HttpClient..ctor")
            && detail.contains("FailingBoot"),
        "the refusal must name the connection and the application code that created its handler: {detail}"
    );
}

#[test]
fn a_dns_lookup_left_in_progress_is_reported_with_where_it_was_started() {
    let detail = refusal(
        r#"{"Failure":"pending-lookup","Name":"localhost"}"#,
        Some(local_network()),
    );
    assert!(
        detail.contains("initialization retains a DNS result")
            && detail.contains("a DNS lookup of localhost that has not finished, started")
            && detail.contains("FailingBoot"),
        "the refusal must name the lookup and the application code that started it: {detail}"
    );
}
