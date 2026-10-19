//! No-body framing sends the response at once; the guest is then told to stop.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, StatusCode};
use isoplane_abi::config::InitSandbox;
use isoplane_abi::dispatch::ResponseHead;

use super::common;

fn guest(status: u16, after_headers: &str) -> Vec<u8> {
    let head = isoplane_abi::encode(&ResponseHead {
        status,
        headers: vec![],
        body: b"not a wire body".to_vec(),
        streaming: true,
    })
    .expect("encode the actual response contract");
    let encoded: String = head.iter().map(|byte| format!("\\{byte:02x}")).collect();
    wat::parse_str(format!(
        r#"(module
        (import "isoplane" "resp_new" (func $response (param i32 i32) (result i32)))
        (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))
        (memory (export "memory") 2 2)
        (data (i32.const 1024) "{encoded}")
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            (drop (call $response (i32.const 1024) (i32.const {})))
            {after_headers})
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64) (i64.const 0))
    )"#,
        head.len()
    ))
    .expect("encode the real no-body guest")
}

async fn request(rig: &common::Rig, method: Method) -> common::Resp {
    common::request(
        rig.proxy,
        Request::builder()
            .method(method)
            .uri("/")
            .header("host", "quotes.isoplane.dev")
            .body(Full::new(Bytes::new()))
            .expect("valid no-body request"),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_body_framing_returns_the_wire_response_and_lets_the_guest_stop() {
    for (method, status) in [
        (Method::GET, 204),
        (Method::GET, 205),
        (Method::GET, 304),
        (Method::HEAD, 200),
    ] {
        let rig = common::start_with_policy(
            guest(
                status,
                "(drop (call $timer (i64.const 600000))) (i64.const 1)",
            ),
            InitSandbox::Sealed,
            None,
            |cfg| cfg.response_queue_bytes = 1,
        )
        .await;
        tokio::time::timeout(Duration::from_secs(30), async {
            let response = request(&rig, method.clone()).await;
            assert_eq!(response.status.as_u16(), status, "{method}/{status} must finish successfully rather than being classified as requester cancellation");
            assert!(response.body.is_empty(), "HTTP no-body framing must suppress the guest's queued payload");
            let cell = &rig.node.real.as_ref().expect("real cells").cells()[0];
            while cell.metrics_raw().dispatch_us.count() == 0 {
                tokio::task::yield_now().await;
            }
            assert_eq!(cell.futures().live_futures(), 0, "the guest's cancellation must end its unused timer wake");
            assert_eq!(cell.metrics().traps, 0, "a guest that stops when told must not trap");
        }).await.expect("the guest must stop once told, without awaiting its unused timer");
        rig.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_same_slice_trap_cannot_be_hidden_by_no_body_response_headers() {
    let rig = common::start_with(guest(204, "unreachable"), InitSandbox::Sealed).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        let response = request(&rig, Method::GET).await;
        assert_eq!(
            response.status,
            StatusCode::BAD_GATEWAY,
            "the proxy must not publish 204 success before the trapping hot slice settles"
        );
        let cell = &rig.node.real.as_ref().expect("real cells").cells()[0];
        assert_eq!(
            cell.metrics_raw().dispatch_us.count(),
            1,
            "a failed no-body response must also wait for dispatch settlement"
        );
        assert_eq!(
            cell.metrics().traps,
            1,
            "the original same-slice trap must remain visible to the host"
        );
    })
    .await
    .expect("a no-body trap must not strand response framing");
    rig.shutdown().await;
}
