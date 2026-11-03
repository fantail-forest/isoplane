//! Response EOF follows the empty-stack dispatch boundary, including adversarial guests.
//! Production daemon policy is qualified on its Linux deployment platform.

#![cfg(target_os = "linux")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use isoplane_abi::dispatch::{HttpRequest, ResponseHead};
use isoplane_abi::host::{CodeTier, Dispatcher, HttpExchange, ResponseDisposition, RouteTarget};
use isoplane_engine::{EngineConfig, IsoplaneEngine};
use isoplane_host::cell::{Cell, CellConfig};
use isoplane_host::{EngineActivator, ModuleId};

async fn response(after_finish: &str) -> (Arc<Cell>, HttpExchange) {
    response_program(
        &format!(
            "(call $finish (call $start (i32.const 1024) (i32.const {{head_len}}))) {after_finish}"
        ),
        "unreachable",
    )
    .await
}

async fn response_program(handle: &str, wake: &str) -> (Arc<Cell>, HttpExchange) {
    response_program_with(CellConfig::for_test("response-boundary"), handle, wake).await
}

async fn response_program_with(
    config: CellConfig,
    handle: &str,
    wake: &str,
) -> (Arc<Cell>, HttpExchange) {
    let head = postcard::to_allocvec(&ResponseHead {
        status: 200,
        headers: vec![],
        body: b"A".to_vec(),
        streaming: true,
    })
    .expect("the response head must encode");
    let encoded = isoplane_test_support::wat_bytes(&head);
    let handle = handle.replace("{head_len}", &head.len().to_string());
    let wasm = wat::parse_str(format!(
        r#"(module
        (import "isoplane" "resp_new" (func $start (param i32 i32) (result i32)))
        (import "isoplane" "resp_finish" (func $finish (param i32)))
        (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))
        (memory (export "memory") 2 2)
        (global $timer_id (mut i64) (i64.const 0))
        (data (i32.const 1024) "{encoded}")
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            {handle})
        (func (export "isoplane_wake") (param $id i64) (param i32 i32) (result i64) {wake})
    )"#
    ))
    .expect("the fixture must encode canonical imports");
    let engine = Arc::new(
        IsoplaneEngine::new(EngineConfig {
            pooling: false,
            ..Default::default()
        })
        .expect("real engine"),
    );
    let module = ModuleId::new("response", "v1");
    let activator =
        Arc::new(EngineActivator::load(engine, module, &wasm).expect("real response guest"));
    let (cell, _) = common::rig_resolving(config, "93.184.216.34:80");
    cell.register_module("owner", activator, common::manifest("response"))
        .expect("register response guest");
    let target = RouteTarget {
        app: "response".into(),
        version: "v1".into(),
        tenant: "owner".into(),
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
async fn finishing_a_response_hands_pending_work_to_background() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cell, mut response) = response_program(
            "(call $finish (call $start (i32.const 1024) (i32.const {head_len}))) (drop (call $timer (i64.const 0))) (i64.const 1)",
            "(i64.const 0)",
        ).await;
        let mut body = vec![];
        while let Some(chunk) = response
            .body
            .read()
            .await
            .expect("the response must settle independently of the background wake")
        {
            body.extend(chunk);
        }
        assert_eq!(
            body, b"A",
            "the completed response must deliver its initial body"
        );
        while cell.metrics_raw().dispatch_us.count() < 2 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            cell.futures().live_futures(),
            0,
            "the background wake must consume the retained timer"
        );
        assert_eq!(cell.resident_ids().len(), 1, "background completion keeps the instance warm");
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

#[tokio::test]
async fn suppressing_a_response_body_keeps_leftover_work_running() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (cell, response) = response_program(
            "(drop (call $start (i32.const 1024) (i32.const {head_len})))
             (global.set $timer_id (call $timer (i64.const 600000))) (i64.const 1)",
            "(if (i64.ne (local.get $id) (global.get $timer_id)) (then unreachable))
             (i64.const 0)",
        )
        .await;
        response
            .body
            .finish(ResponseDisposition::NoBody)
            .await
            .expect("successful body suppression");
        let ids = cell.resident_ids();
        assert_eq!(ids.len(), 1);
        let instance = ids[0];
        let slot = cell.slot(instance).expect("response instance");
        let dispatch = slot.ctx.dispatch_id();
        let timers = cell
            .futures()
            .pending_of_dispatch(instance, dispatch, "clock_deadline_in");
        assert_eq!(timers.len(), 1);
        cell.complete_future(timers[0], isoplane_abi::result::ResultRecord::Ok(vec![]));
        while cell.metrics_raw().dispatch_us.count() == 0 {
            tokio::task::yield_now().await;
        }
        assert!(
            cell.slot(instance).is_some(),
            "body suppression must not cancel the guest and trigger its trapping cancellation wake"
        );
        assert_eq!(cell.metrics().cancellation_grace_kills, 0);
        assert_eq!(cell.futures().live_futures(), 0);
        cell.shutdown();
    })
    .await
    .expect("bodyless response and leftover work must settle");
}

#[tokio::test]
async fn a_nonreturning_background_callback_is_killed_after_its_request_completes() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let config = CellConfig {
            slice_wall_ms_cap: 50,
            ..CellConfig::for_test("background-hard-stop")
        };
        let (cell, mut response) = response_program_with(config,
            "(call $finish (call $start (i32.const 1024) (i32.const {head_len}))) (drop (call $timer (i64.const 0))) (i64.const 1)",
            "(loop $spin (br $spin)) (i64.const 0)",
        ).await;
        let mut body = Vec::new();
        while let Some(chunk) = response.body.read().await.expect("completed response") {
            body.extend(chunk);
        }
        assert_eq!(body, b"A");
        while !cell.resident_ids().is_empty() {
            tokio::task::yield_now().await;
        }
        assert_eq!(cell.metrics().budget_kills, 1);
        assert_eq!(cell.futures().live_futures(), 0);
        cell.shutdown();
    }).await.expect("the background hard stop must evict the spinning instance");
}
