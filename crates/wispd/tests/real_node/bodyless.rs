//! No-body framing waits for real dispatch settlement and never resumes guest work.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, StatusCode};
use wisp_abi::config::InitSandbox;
use wisp_abi::dispatch::ResponseHead;

use super::common;

fn guest(status: u16, after_headers: &str) -> Vec<u8> {
    let head = wisp_abi::encode(&ResponseHead {
        status,
        headers: vec![],
        body: b"not a wire body".to_vec(),
        streaming: true,
    })
    .expect("encode the actual response contract");
    let encoded: String = head.iter().map(|byte| format!("\\{byte:02x}")).collect();
    wat::parse_str(format!(
        r#"(module
        (import "wisp" "resp_new" (func $response (param i32 i32) (result i32)))
        (import "wisp" "clock_deadline_in" (func $timer (param i64) (result i64)))
        (memory (export "memory") 2 2)
        (data (i32.const 1024) "{encoded}")
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "wisp_handle") (param i32 i32) (result i64)
            (drop (call $response (i32.const 1024) (i32.const {})))
            {after_headers})
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) unreachable)
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
            .header("host", "quotes.wisp.dev")
            .body(Full::new(Bytes::new()))
            .expect("valid no-body request"),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_body_framing_completes_the_dispatch_before_returning_the_wire_response() {
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
            assert_eq!(cell.futures().live_futures(), 0, "the wire response cannot complete while the guest retains a timer wake");
            assert_eq!(cell.metrics_raw().request_us.count(), 1, "the host must acknowledge one settled dispatch before returning the no-body response");
            assert!(cell.tombstoned_instances().is_empty(), "no-body completion must not enter the guest's trapping wake");
        }).await.expect("no-body completion must not await the unused guest timer");
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
            cell.metrics_raw().request_us.count(),
            1,
            "a failed no-body response must also wait for dispatch settlement"
        );
        assert_eq!(
            cell.tombstoned_instances().len(),
            1,
            "the original same-slice trap must remain visible to the host"
        );
    })
    .await
    .expect("a no-body trap must not strand response framing");
    rig.shutdown().await;
}
