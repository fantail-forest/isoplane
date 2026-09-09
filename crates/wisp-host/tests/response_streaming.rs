//! Response EOF follows the empty-stack dispatch boundary, including adversarial guests.

mod common;

use std::sync::Arc;
use std::time::Duration;

use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_abi::dispatch::{HttpRequest, ResponseHead};
use wisp_abi::host::{CodeTier, Dispatcher, HttpExchange, RouteTarget};
use wisp_engine::{EngineConfig, WispEngine};
use wisp_host::cell::{Cell, CellConfig};
use wisp_host::{EngineActivator, ModuleId};

async fn response(after_finish: &str) -> (Arc<Cell>, HttpExchange) {
    let head = postcard::to_allocvec(&ResponseHead {
        status: 200,
        headers: vec![],
        body: b"A".to_vec(),
        streaming: true,
    })
    .expect("the response head must encode");
    let encoded: String = head.iter().map(|byte| format!("\\{byte:02x}")).collect();
    let wasm = wat::parse_str(format!(
        r#"(module
        (import "wisp" "resp_new" (func $start (param i32 i32) (result i32)))
        (import "wisp" "resp_finish" (func $finish (param i32)))
        (import "wisp" "clock_deadline_in" (func $timer (param i64) (result i64)))
        (memory (export "memory") 2 2)
        (data (i32.const 1024) "{encoded}")
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "wisp_handle") (param i32 i32) (result i64)
            (call $finish (call $start (i32.const 1024) (i32.const {})))
            {after_finish})
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) unreachable)
    )"#,
        head.len()
    ))
    .expect("the fixture must encode canonical imports");
    let engine = Arc::new(
        WispEngine::new(EngineConfig {
            pooling: false,
            ..Default::default()
        })
        .expect("real engine"),
    );
    let module = ModuleId::new("response", "v1");
    let activator =
        Arc::new(EngineActivator::load(engine, module, &wasm).expect("real response guest"));
    let (cell, _) = common::rig_resolving(
        CellConfig::for_test("response-boundary"),
        "93.184.216.34:80",
    );
    cell.register_module(
        "owner",
        activator,
        common::manifest("response", HandlerLifecycle::Reuse, Limits::default()),
    )
    .expect("register response guest");
    let target = RouteTarget {
        app: "response".into(),
        version: "v1".into(),
        tenant: "owner".into(),

        lifecycle: HandlerLifecycle::Reuse,
        tier: CodeTier::Fast,
    };
    let exchange = cell
        .dispatch_http_stream(
            &target,
            HttpRequest {
                method: "GET".into(),
                url: "http://app.example/".into(),
                path: "/".into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            },
        )
        .await
        .expect("response headers");
    (cell, exchange)
}

#[tokio::test]
async fn finishing_a_response_then_parking_cannot_resume_guest_work() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cell, mut response) =
            response("(drop (call $timer (i64.const 0))) (i64.const 1)").await;
        let mut body = vec![];
        while let Some(chunk) = response
            .body
            .read()
            .await
            .expect("the finished response must not enter the trapping wake")
        {
            body.extend(chunk);
        }
        assert_eq!(
            body, b"A",
            "the completed response must deliver its initial body"
        );
        while cell.metrics_raw().request_us.count() == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            cell.futures().live_futures(),
            0,
            "response settlement must cancel the unused timer before any wake"
        );
        cell.shutdown();
    })
    .await
    .expect("response completion must not hang");
}

#[tokio::test]
async fn a_trap_after_finish_in_the_same_hot_slice_cannot_publish_successful_eof() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cell, mut response) = response("unreachable").await;
        loop {
            match response.body.read().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("resp_finish must not publish successful EOF before the trapping hot slice settles"),
                Err(_) => break,
            }
        }
        cell.shutdown();
    }).await.expect("the trapping response must not hang");
}
