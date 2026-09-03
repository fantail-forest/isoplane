//! A real node: real engine, cells, guest wasm, proxy and TCP.

#![cfg(feature = "real-backends")]

mod common;

use common::{config, get, manifest, serve, start_with, Rig};
use hyper::StatusCode;
use wisp_abi::config::InitSandbox;
use wisp_abi::dispatch::ResponseHead;
use wispd::node::Node;

/// A guest that answers every dispatch with one canned response, its
/// postcard-encoded head dropped in as a data segment.
fn guest(head: &ResponseHead) -> Vec<u8> {
    let encoded = wisp_abi::encode(head).expect("encoding a response head");
    let escaped: String = encoded.iter().map(|b| format!("\\{b:02x}")).collect();
    let len = encoded.len();
    let src = format!(
        r#"
(module
  (import "wisp" "resp_new" (func $resp_new (param i32 i32) (result i32)))
  (memory (export "memory") 2 2)
  (data (i32.const 4096) "{escaped}")
  (global $bump (export "g_bump") (mut i32) (i32.const 16384))

  (func (export "wisp_alloc") (param $size i32) (param $align i32) (result i32)
    (local $p i32)
    (if (i32.gt_u (i32.add (global.get $bump) (local.get $size)) (i32.const 122880))
      (then (global.set $bump (i32.const 16384))))
    (local.set $p (global.get $bump))
    (global.set $bump
      (i32.and (i32.add (i32.add (local.get $p) (local.get $size)) (i32.const 15))
               (i32.const -16)))
    (local.get $p))

  ;; PollStatus::Done {{ response: h }} packs as (h as u64) << 32.
  (func (export "wisp_handle") (param $ptr i32) (param $len i32) (result i64)
    (i64.shl
      (i64.extend_i32_u (call $resp_new (i32.const 4096) (i32.const {len})))
      (i64.const 32)))

  (func (export "wisp_wake") (param $future i64) (param $ptr i32) (param $len i32) (result i64)
    (i64.const 0))
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

    let r = get(rig.proxy, "quotes.wisp.dev", "/api/quote").await;
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
            r.headers.get("x-wisp-version").unwrap().to_str().unwrap(),
            r.headers.get("x-wisp-tier").unwrap().to_str().unwrap()
        ),
        ("v1", "fast"),
        "the platform headers are the proxy's"
    );
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_guest_chosen_status_code_reaches_the_client() {
    let rig = start(head(418, "no coffee")).await;
    let r = get(rig.proxy, "quotes.wisp.dev", "/brew").await;
    assert_eq!(r.status, 418, "the guest chose the status");
    assert_eq!(r.body, "no coffee", "and the body");
    rig.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_reports_measurements_from_served_requests() {
    let rig = start(head(200, "ok")).await;
    for i in 0..20 {
        assert_eq!(
            get(rig.proxy, "quotes.wisp.dev", &format!("/{i}"))
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
        metric("wisp_proxy_requests_total"),
        Some(20.0),
        "the proxy counted the requests"
    );
    assert_eq!(
        metric("wisp_cell_dispatches_total"),
        Some(20.0),
        "the cell counted the same 20 dispatches the proxy did"
    );
    assert!(
        metric("wisp_cpu_ms_total").unwrap() > 0.0,
        "twenty dispatches of real guest code recorded some CPU: {text}"
    );
    assert!(!text.contains("NaN"), "nothing renders as NaN");
    rig.shutdown().await;
}

/// Configuration supplied during preparation is observed by the serving guest.
#[tokio::test]
async fn a_real_guests_init_visible_config_is_consumed_at_preinit_not_at_runtime() {
    let path = wisp_fixtures::rust_path("quotes-api");

    let dir = tempfile::tempdir().expect("a temporary directory");
    let mut cfg = config(dir.path());
    cfg.apps[0].versions[0].wasm = Some(path);
    cfg.apps[0].init_visible_vars = [("REGION".to_string(), "nz-akl".to_string())]
        .into_iter()
        .collect();

    let node = Node::build(cfg).expect("a real node must assemble");

    let rec = node
        .deploys
        .lock()
        .first()
        .cloned()
        .expect("startup deploys are recorded");
    assert!(
        rec.pipeline_ran,
        "the daemon must run the pipeline, not register raw"
    );
    assert!(
        rec.from_template,
        "a guest that exports wisp_initialize must yield a template"
    );
    assert!(
        rec.stages.iter().any(|s| s.name == "preinit"),
        "the startup deploy ran the pipeline: {:?}",
        rec.stages
    );
    let pre = rec
        .preinit
        .as_ref()
        .expect("a template implies a pre-init summary");
    let reasons: Vec<String> = pre["deferred_reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d[1].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        reasons,
        vec!["clock_now_coarse".to_string()],
        "exactly the clock-reading initializer defers"
    );
    assert_eq!(
        pre["init_pal"], "sealed",
        "a Rust SDK guest needs no runtime PAL"
    );

    let rig = serve(node, dir).await;

    let r = get(rig.proxy, "quotes.wisp.dev", "/memo").await;
    assert_eq!(r.status, StatusCode::OK, "body: {}", r.body);
    assert!(
        !r.body.contains("capability_violation"),
        "the init-visible imports must have been consumed at deploy time: {}",
        r.body
    );
    assert!(
        r.body.contains("\"config_region\":\"nz-akl\""),
        "the deploy-time config is inside the template: {}",
        r.body
    );

    rig.shutdown().await;
}
