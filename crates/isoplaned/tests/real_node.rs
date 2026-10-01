//! A real node: real engine, cells, guest wasm, proxy and TCP.

#![cfg(feature = "real-backends")]

mod common;
#[path = "real_node/egress.rs"]
mod egress;
#[cfg(target_os = "linux")]
#[path = "real_node/mapped_storage.rs"]
mod mapped_storage;

use common::preparation::LocalDeployment;
use common::{config, get, manifest, serve, start_with, Rig};
use isoplane_abi::config::InitSandbox;
use isoplane_abi::dispatch::ResponseHead;
use isoplaned::node::Node;

/// A guest that answers every dispatch with one canned response, its
/// postcard-encoded head dropped in as a data segment.
fn guest(head: &ResponseHead) -> Vec<u8> {
    guest_with_functions(head, "", "")
}

fn guest_with_functions(head: &ResponseHead, functions: &str, before_response: &str) -> Vec<u8> {
    let encoded = isoplane_abi::encode(head).expect("encoding a response head");
    let escaped: String = encoded.iter().map(|b| format!("\\{b:02x}")).collect();
    let len = encoded.len();
    let src = format!(
        r#"
(module
  (import "isoplane" "resp_new" (func $resp_new (param i32 i32) (result i32)))
  (memory (export "memory") 2 2)
  (data (i32.const 4096) "{escaped}")
  (global $bump (export "g_bump") (mut i32) (i32.const 16384))

  (func (export "isoplane_alloc") (param $size i32) (param $align i32) (result i32)
    (local $p i32)
    (if (i32.gt_u (i32.add (global.get $bump) (local.get $size)) (i32.const 122880))
      (then (global.set $bump (i32.const 16384))))
    (local.set $p (global.get $bump))
    (global.set $bump
      (i32.and (i32.add (i32.add (local.get $p) (local.get $size)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))

  ;; PollStatus::Done {{ response: h }} packs as (h as u64) << 32.
  (func (export "isoplane_handle") (param $ptr i32) (param $len i32) (result i64)
    {before_response}
    (i64.shl
      (i64.extend_i32_u (call $resp_new (i32.const 4096) (i32.const {len})))
      (i64.const 32)))

  (func (export "isoplane_wake") (param $future i64) (param $ptr i32) (param $len i32) (result i64)
    (i64.const 0))
  {functions}
)
"#
    );
    wat::parse_str(&src).expect("fixture .wat must assemble")
}

fn head(status: u16, body: &str) -> ResponseHead {
    ResponseHead {
        status,
        headers: vec![("content-type".into(), "text/plain".into())],
        body: body.as_bytes().to_vec(),
        streaming: false,
    }
}

async fn start(head: ResponseHead) -> Rig {
    start_with(guest(&head), InitSandbox::Sealed).await
}

// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_http_request_runs_real_guest_wasm_end_to_end() {
    let rig = start(head(200, "hello from wasm")).await;

    let r = get(rig.proxy, "quotes.isoplane.dev", "/api/quote").await;
    assert_eq!(r.status, 200, "the guest's status reaches the client");
    assert_eq!(
        r.body, "hello from wasm",
        "the body came out of the guest's linear memory"
    );
    assert_eq!(
        r.headers.get("content-type").unwrap(),
        "text/plain",
        "and so did the headers"
    );
    assert_eq!(
        (
            r.headers
                .get("x-isoplane-version")
                .unwrap()
                .to_str()
                .unwrap(),
            r.headers.get("x-isoplane-tier").unwrap().to_str().unwrap()
        ),
        ("v1", "fast"),
        "the platform headers are the proxy's"
    );
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_guest_chosen_status_code_reaches_the_client() {
    let rig = start(head(418, "no coffee")).await;
    let r = get(rig.proxy, "quotes.isoplane.dev", "/brew").await;
    assert_eq!(r.status, 418, "the guest chose the status");
    assert_eq!(r.body, "no coffee", "and the body");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_reports_measurements_from_served_requests() {
    let rig = start(head(200, "ok")).await;
    for i in 0..20 {
        assert_eq!(
            get(rig.proxy, "quotes.isoplane.dev", &format!("/{i}"))
                .await
                .status,
            200,
            "every request is served"
        );
    }

    let text = get(rig.admin, "localhost", "/metrics").await.body;

    let metric = |name: &str| -> Option<f64> {
        text.lines()
            .filter(|l| !l.starts_with('#'))
            .find(|l| l.starts_with(name))
            .and_then(|l| l.rsplit(' ').next())
            .and_then(|v| v.parse().ok())
    };
    assert_eq!(
        metric("isoplane_proxy_requests_total"),
        Some(20.0),
        "the proxy counted the requests"
    );
    assert_eq!(
        metric("isoplane_cell_dispatches_total"),
        Some(20.0),
        "the cell counted the same 20 dispatches the proxy did"
    );
    assert!(
        metric("isoplane_occupancy_ms_total").unwrap() > 0.0,
        "twenty dispatches of real guest code recorded some occupancy: {text}"
    );
    assert!(!text.contains("NaN"), "nothing renders as NaN");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_csharp_handler_receives_request_fields_for_every_standard_http_method() {
    use base64::Engine as _;
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Request;

    let rig = start_with(
        common::preparation::Guest::from("isoplane_guest_dotnet_request"),
        InitSandbox::RuntimeBringUp,
    )
    .await;
    // CONNECT's authority-form target has no path; this route also admits OPTIONS *.
    rig.node
        .routes
        .add_route("quotes.isoplane.dev", "", "quotes")
        .unwrap();
    let base64 = base64::engine::general_purpose::STANDARD;
    for method in [
        "GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "TRACE", "CONNECT",
    ] {
        for populated in [false, true] {
            let (uri, path, query) = if method == "CONNECT" {
                ("quotes.isoplane.dev:443", "", "")
            } else if method == "OPTIONS" && !populated {
                ("*", "*", "")
            } else if populated {
                (
                    "/probe?tag=one&tag=two&empty=&flag&encoded=%E2%98%83",
                    "/probe",
                    "tag=one\ntag=two\nempty=\nflag=\nencoded=%E2%98%83\n",
                )
            } else {
                ("/probe", "/probe", "")
            };
            let body: &[u8] = if populated && !matches!(method, "CONNECT" | "TRACE") {
                b"\0\xff{\"message\":\"hello\"}\r\n"
            } else {
                b""
            };
            let headers = if populated { "first\nsecond\n" } else { "" };
            let expected = base64.encode(format!(
                "{method}\n{uri}\n{path}\n{query}{headers}{}",
                base64.encode(body)
            ));
            let mut builder = Request::builder()
                .method(method)
                .uri(uri)
                .header("host", "quotes.isoplane.dev")
                .header("x-expected", &expected);
            if populated {
                builder = builder
                    .header("X-Probe", "first")
                    .header("x-probe", "second");
            }
            let req = builder
                .body(Full::new(Bytes::copy_from_slice(body)))
                .unwrap();
            let response = common::request(rig.proxy, req).await;
            assert_eq!(response.status, 200,
                "the C# handler must receive every supplied field for {method} (populated={populated}): {}", response.body);
            if !matches!(method, "HEAD" | "CONNECT") {
                assert_eq!(
                    response.body, expected,
                    "the handler must preserve request bytes and repeated fields for {method}"
                );
            }
        }
    }
    let streamed = Request::builder()
        .method("POST")
        .uri("/streaming")
        .header("host", "quotes.isoplane.dev")
        .body(Full::new(Bytes::from_static(b"selected route")))
        .expect("mixed streaming route request");
    let streamed = common::request(rig.proxy, streamed).await;
    assert_eq!(
        streamed.status, 200,
        "the same router must explicitly select its streaming handler"
    );
    assert_eq!(
        streamed.body,
        format!("stream:{}", base64.encode(b"selected route")),
        "streaming selection must preserve upload bytes without changing buffered routes"
    );
    let invalid = Request::builder()
        .uri("/probe")
        .header("host", "quotes.isoplane.dev")
        .header("x-expected", "deliberately-wrong")
        .body(Full::new(Bytes::new()))
        .unwrap();
    assert_eq!(common::request(rig.proxy, invalid).await.status, 422,
        "the fixture must reject mismatched request fields, including those checked only by status for HEAD and CONNECT");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guest_connection_headers_cannot_control_the_http_response() {
    let rig = start(ResponseHead {
        status: 200,
        headers: vec![
            ("Connection".into(), "X-Private, x-isoplane-version".into()),
            ("Connection".into(), "X-Other".into()),
            ("x-private".into(), "discard".into()),
            ("x-other".into(), "discard".into()),
            ("keep-alive".into(), "timeout=5".into()),
            ("proxy-connection".into(), "keep-alive".into()),
            ("proxy-authenticate".into(), "Basic".into()),
            ("proxy-authorization".into(), "Basic proxy-only".into()),
            ("te".into(), "trailers".into()),
            ("trailer".into(), "x-checksum".into()),
            ("upgrade".into(), "other-protocol".into()),
            ("transfer-encoding".into(), "chunked".into()),
            ("content-length".into(), "9999".into()),
            ("content-length".into(), "1".into()),
            ("set-cookie".into(), "a=1; HttpOnly".into()),
            ("set-cookie".into(), "b=2; Secure".into()),
            ("content-type".into(), "text/plain".into()),
        ],
        body: b"hello".to_vec(),
        streaming: false,
    })
    .await;
    let response = get(rig.proxy, "quotes.isoplane.dev", "/").await;
    assert_eq!(
        response.status, 200,
        "the proxy must serve a guest response with host-owned framing"
    );
    assert_eq!(
        response.body, "hello",
        "guest length declarations must not truncate the body or make the client wait"
    );
    assert_eq!(
        response.headers["content-length"], "5",
        "the actual body determines wire length"
    );
    for name in [
        "connection",
        "x-private",
        "x-other",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "upgrade",
        "transfer-encoding",
    ] {
        assert!(
            !response.headers.contains_key(name),
            "guest connection-specific {name} must not reach the client"
        );
    }
    assert_eq!(
        response.headers["x-isoplane-version"], "v1",
        "Connection cannot strip authoritative platform metadata"
    );
    let cookies: Vec<_> = response
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    assert_eq!(
        cookies,
        ["a=1; HttpOnly", "b=2; Secure"],
        "Set-Cookie values must remain separate and ordered"
    );
    assert_eq!(
        response.headers["content-type"], "text/plain",
        "application response headers must survive filtering"
    );
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn response_framing_respects_methods_and_statuses_that_forbid_content() {
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Request;

    for (method, status, expected_length) in [
        ("HEAD", 200, None),
        ("GET", 204, None),
        ("GET", 205, Some("0")),
        ("GET", 304, None),
        ("CONNECT", 200, None),
    ] {
        let rig = start(ResponseHead {
            status,
            headers: vec![
                ("content-length".into(), "9999".into()),
                ("transfer-encoding".into(), "chunked".into()),
            ],
            body: b"must not be sent".to_vec(),
            streaming: false,
        })
        .await;
        rig.node
            .routes
            .add_route("quotes.isoplane.dev", "", "quotes")
            .unwrap();
        let uri = if method == "CONNECT" {
            "quotes.isoplane.dev:443"
        } else {
            "/"
        };
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "quotes.isoplane.dev")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = common::request(rig.proxy, req).await;
        assert_eq!(
            response.status.as_u16(),
            status,
            "framing must preserve the final status for {method}"
        );
        assert!(
            response.body.is_empty(),
            "{method} {status} must not transmit guest content"
        );
        assert_eq!(
            response
                .headers
                .get("content-length")
                .map(|value| value.to_str().unwrap()),
            expected_length,
            "{method} {status} must not send an unverified or prohibited length"
        );
        assert!(
            !response.headers.contains_key("transfer-encoding"),
            "{method} {status} must not inherit guest transfer framing"
        );
        rig.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn csharp_deployment_variables_are_captured_before_requests_run() {
    let wasm = common::preparation::Guest::from("isoplane_guest_dotnet_config");
    let directory = tempfile::tempdir().expect("a unique node directory");
    let node = Node::build(common::config(directory.path())).expect("the real node must start");
    let mut manifest = common::manifest("quotes");
    manifest.init_visible_vars = vec![
        ("REGION".into(), "Tāmaki Makaurau".into()),
        ("PORT".into(), "17481".into()),
        ("REGION".into(), String::new()),
    ];
    node.real
        .as_ref()
        .expect("the node must use real backends")
        .deploy_locally(
            "quotes",
            "v1",
            "acme",
            manifest,
            &wasm,
            InitSandbox::RuntimeBringUp,
        )
        .await
        .expect("deployment configuration must be readable during pre-initialization");
    let rig = common::serve(node, directory).await;
    for path in ["/first", "/second"] {
        let response = common::get(rig.proxy, "quotes.isoplane.dev", path).await;
        assert_eq!(
            response.status,
            hyper::StatusCode::OK,
            "the activated C# handler must not call the init-only configuration import"
        );
        assert_eq!(response.body, "REGION=Tāmaki Makaurau\nPORT=17481\nREGION=\n", "captured deployment variables must preserve Unicode, order, duplicate keys and empty values");
    }
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn managed_libraries_observe_a_read_only_disk_holding_only_the_platform_files() {
    let rig = start_with(
        common::preparation::Guest::from("isoplane_guest_dotnet_filesystem"),
        InitSandbox::RuntimeBringUp,
    )
    .await;
    let mut mismatches = Vec::new();
    // The platform serves its trust bundle and the application's managed package.
    for (path, expected) in [
        ("/root", "/|True|/.isoplane,/etc"),
        ("/missing", "False|False|False"),
        ("/read", "FileNotFoundException:80070002"),
        ("/read-root-file", "FileNotFoundException:80070002"),
        ("/resolve", "/missing"),
        ("/enumerate-missing", "DirectoryNotFoundException:80070003"),
        ("/write", "IOException:00000045"),
        ("/mkdir", "IOException:00000045"),
        ("/delete", "delete succeeded"),
        ("/tempfile", "IOException:00000045"),
        ("/symlink", "IOException:00000045"),
        ("/temp", "/tmp/|False"),
        ("/resource", "trusted embedded resource"),
        ("/assembly", "System.Private.CoreLib"),
    ] {
        let response = get(rig.proxy, "quotes.isoplane.dev", path).await;
        if response.status != hyper::StatusCode::OK || response.body != expected {
            mismatches.push(format!(
                "{path}: status {}; expected {expected:?}, got {:?}",
                response.status, response.body
            ));
        }
    }
    rig.shutdown().await;
    assert!(
        mismatches.is_empty(),
        "managed filesystem contract failures: {mismatches:#?}"
    );
}
#[path = "real_node/aspnet.rs"]
mod aspnet;

#[path = "real_node/streaming.rs"]
mod streaming;

#[path = "common/owned_endpoint.rs"]
mod owned_endpoint;

#[path = "real_node/bodyless.rs"]
mod bodyless;

#[path = "real_node/app_identity.rs"]
mod app_identity;
