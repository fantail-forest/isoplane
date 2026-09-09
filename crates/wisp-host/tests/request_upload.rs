//! Request-body imports through the real WASM engine and dispatch context.

mod common;

use std::sync::Arc;
use std::time::Duration;
use wisp_abi::config::{HandlerLifecycle, Limits};
use wisp_abi::dispatch::HttpRequest;
use wisp_abi::result::{FutureError, ResultRecord};
use wisp_engine::{Completion, EngineConfig, WispEngine};
use wisp_host::cell::{CellConfig, Target};
use wisp_host::{EngineActivator, ModuleId};

#[test]
fn request_body_reads_reject_zero_and_negative_sizes_through_the_real_import() {
    let denied = postcard::to_allocvec(&ResultRecord::Err(FutureError::Denied(String::new())))
        .expect("canonical denied result");
    for max_bytes in [0, -1] {
        let wasm = wat::parse_str(format!(
            r#"(module
          (import "wisp" "req_body_read" (func $read (param i32) (result i64)))
          (memory (export "memory") 2 2)
          (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 4096))
          (func (export "wisp_handle") (param i32 i32) (result i64)
            (drop (call $read (i32.const {max_bytes}))) (i64.const 1))
          (func (export "wisp_wake") (param i64) (param $ptr i32) (param i32) (result i64)
            (if (i32.ne (i32.load8_u (local.get $ptr)) (i32.const {})) (then unreachable))
            (if (i32.ne (i32.load8_u offset=1 (local.get $ptr)) (i32.const {})) (then unreachable))
            (i64.const 0))
        )"#,
            denied[0], denied[1]
        ))
        .expect("the hostile read-size fixture must encode");
        let engine = Arc::new(
            WispEngine::new(EngineConfig {
                pooling: false,
                ..Default::default()
            })
            .expect("real engine"),
        );
        let module = ModuleId::new("read-size", "v1");
        let activator = Arc::new(
            EngineActivator::load(engine, module.clone(), &wasm)
                .expect("optional request import must link"),
        );
        assert!(
            activator.template().imports("wisp", "req_body_read"),
            "the compiled import must advertise upload delivery capability"
        );
        let (cell, _) =
            common::rig_resolving(CellConfig::for_test("read-size"), "93.184.216.34:80");
        cell.register_module(
            "owner",
            activator,
            common::manifest("read-size", HandlerLifecycle::Reuse, Limits::default()),
        )
        .expect("register hostile read guest");
        let request = cell.http_record(
            &module,
            HttpRequest {
                method: "POST".into(),
                url: "http://upload.test/".into(),
                path: "/".into(),
                query: vec![],
                headers: vec![],
                body: vec![],
            },
        );
        let result = cell
            .dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                request,
                Duration::from_secs(10),
            )
            .expect("invalid read must settle");
        assert_eq!(result.completion, Completion::Done, "read size {max_bytes} must return the canonical Denied result instead of EOF or a huge read");
        cell.shutdown();
    }
}

struct UnreadUpload(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for UnreadUpload {
    fn drop(&mut self) {
        if let Some(dropped) = self.0.take() {
            dropped
                .send(())
                .expect("the test must observe upload disposal");
        }
    }
}
#[async_trait::async_trait]
impl wisp_abi::host::HttpBody for UnreadUpload {
    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        panic!("an early response must not drain unread upload content")
    }
}

fn response_cell(imports: &str, before_response: &str) -> Arc<wisp_host::Cell> {
    use wisp_abi::dispatch::ResponseHead;
    let head = postcard::to_allocvec(&ResponseHead {
        status: 204,
        headers: vec![],
        body: vec![],
        streaming: false,
    })
    .expect("canonical early response");
    let encoded: String = head.iter().map(|byte| format!("\\{byte:02x}")).collect();
    let wasm = wat::parse_str(format!(r#"(module
        {imports}
        (import "wisp" "resp_new" (func $response (param i32 i32) (result i32)))
        (memory (export "memory") 2 2)
        (data (i32.const 1024) "{encoded}")
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "wisp_handle") (param $ptr i32) (param $len i32) (result i64)
            {before_response}
            (i64.shl (i64.extend_i32_u (call $response (i32.const 1024) (i32.const {}))) (i64.const 32)))
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) unreachable)
    )"#, head.len())).expect("early response fixture");
    let engine = Arc::new(
        WispEngine::new(EngineConfig {
            pooling: false,
            ..Default::default()
        })
        .expect("real engine"),
    );
    let module = ModuleId::new("early-upload", "v1");
    let activator =
        Arc::new(EngineActivator::load(engine, module, &wasm).expect("upload-aware guest"));
    let (cell, _) = common::rig_resolving(CellConfig::for_test("early-upload"), "93.184.216.34:80");
    cell.register_module(
        "owner",
        activator,
        common::manifest("early-upload", HandlerLifecycle::Reuse, Limits::default()),
    )
    .expect("register early response guest");
    cell
}

#[tokio::test]
async fn an_early_response_releases_the_unread_upload_at_dispatch_settlement() {
    use wisp_abi::host::{CodeTier, Dispatcher, HttpUpload, RouteTarget};
    let cell = response_cell(
        r#"(import "wisp" "req_body_read" (func $read (param i32) (result i64)))"#,
        "",
    );
    let (dropped, disposal) = tokio::sync::oneshot::channel();
    let mut response = cell
        .dispatch_http_upload(
            &RouteTarget {
                app: "early-upload".into(),
                version: "v1".into(),
                tenant: "owner".into(),

                lifecycle: HandlerLifecycle::Reuse,
                tier: CodeTier::Fast,
            },
            HttpUpload {
                request: HttpRequest {
                    method: "POST".into(),
                    url: "http://upload.test/".into(),
                    path: "/".into(),
                    query: vec![],
                    headers: vec![],
                    body: vec![],
                },
                body: Box::new(UnreadUpload(Some(dropped))),
                can_have_body: true,
            },
        )
        .await
        .expect("early response headers");
    assert_eq!(
        response.status, 204,
        "the guest can respond without consuming the upload"
    );
    assert!(
        response
            .body
            .read()
            .await
            .expect("early response EOF")
            .is_none(),
        "the response has no body"
    );
    tokio::time::timeout(Duration::from_secs(10), disposal)
        .await
        .expect("dispatch settlement must promptly release unread upload ownership")
        .expect("the actual upload body must be dropped before cell shutdown");
    assert_eq!(
        cell.futures().live_futures(),
        0,
        "early response leaves no upload read tasks behind"
    );
    cell.shutdown();
}

struct UploadChunks(std::collections::VecDeque<Vec<u8>>);
#[async_trait::async_trait]
impl wisp_abi::host::HttpBody for UploadChunks {
    async fn read(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.0.pop_front())
    }
}

#[tokio::test]
async fn a_guest_without_upload_imports_receives_the_complete_inline_body() {
    use wisp_abi::host::{CodeTier, Dispatcher, HttpUpload, RouteTarget};
    let cell = response_cell(
        "",
        r#"
        (block $found
          (loop $scan
            (if (i32.lt_u (local.get $len) (i32.const 5)) (then unreachable))
            (br_if $found (i32.and
              (i32.eq (i32.load8_u (local.get $ptr)) (i32.const 88))
              (i32.eq (i32.load offset=1 (local.get $ptr)) (i32.const 1145258561))))
            (local.set $ptr (i32.add (local.get $ptr) (i32.const 1)))
            (local.set $len (i32.sub (local.get $len) (i32.const 1)))
            (br $scan)))
    "#,
    );
    let mut response = cell
        .dispatch_http_upload(
            &RouteTarget {
                app: "early-upload".into(),
                version: "v1".into(),
                tenant: "owner".into(),

                lifecycle: HandlerLifecycle::Reuse,
                tier: CodeTier::Fast,
            },
            HttpUpload {
                request: HttpRequest {
                    method: "POST".into(),
                    url: "http://upload.test/".into(),
                    path: "/".into(),
                    query: vec![],
                    headers: vec![],
                    body: b"X".to_vec(),
                },
                body: Box::new(UploadChunks([b"AB".to_vec(), b"CD".to_vec()].into())),
                can_have_body: true,
            },
        )
        .await
        .expect("legacy guest must receive prefix plus every upload chunk");
    assert_eq!(
        response.status, 204,
        "a legacy guest must find XABCD in the original inline request record"
    );
    assert!(
        response
            .body
            .read()
            .await
            .expect("legacy response completes")
            .is_none(),
        "the legacy response remains bodyless"
    );
    cell.shutdown();
}

#[tokio::test]
async fn a_metadata_only_guest_distinguishes_unknown_length_empty_uploads_from_bodyless_requests() {
    use wisp_abi::host::{CodeTier, Dispatcher, HttpUpload, RouteTarget};
    for can_have_body in [true, false] {
        let cell = response_cell(
            r#"(import "wisp" "req_body_can_have_body" (func $has_body (result i32)))"#,
            &format!(
                "(if (i32.ne (call $has_body) (i32.const {})) (then unreachable))",
                u8::from(can_have_body)
            ),
        );
        let mut response = cell
            .dispatch_http_upload(
                &RouteTarget {
                    app: "early-upload".into(),
                    version: "v1".into(),
                    tenant: "owner".into(),

                    lifecycle: HandlerLifecycle::Reuse,
                    tier: CodeTier::Fast,
                },
                HttpUpload {
                    request: HttpRequest {
                        method: "POST".into(),
                        url: "http://upload.test/".into(),
                        path: "/".into(),
                        query: vec![],
                        headers: vec![],
                        body: vec![],
                    },
                    body: Box::new(UploadChunks(Default::default())),
                    can_have_body,
                },
            )
            .await
            .expect("metadata-only import must preserve original request framing after buffering");
        assert_eq!(
            response.status, 204,
            "the guest must distinguish empty unknown-length content from a bodyless request"
        );
        assert!(
            response
                .body
                .read()
                .await
                .expect("metadata response completes")
                .is_none(),
            "the metadata probe response remains bodyless"
        );
        cell.shutdown();
    }
}
