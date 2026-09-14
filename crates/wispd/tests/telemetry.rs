#![cfg(feature = "real-backends")]
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;

#[path = "common/preparation.rs"]
mod preparation;
use preparation::LocalDeployment;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
#[path = "common/otlp.rs"]
mod otlp;
use otlp::{field, Collector};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_actual_http_guest_completion_crosses_the_otlp_boundary() {
    use wisp_abi::config::{AppManifest, InitSandbox};
    use wisp_abi::dispatch::ResponseHead;

    let mut collector = Collector::<ExportLogsServiceRequest>::start(StatusCode::OK).await;
    let mut traces = Collector::<ExportTraceServiceRequest>::start(StatusCode::OK).await;
    let trace_endpoint = traces
        .endpoint
        .parse::<wispd::logs::OtlpEndpoint>()
        .unwrap();
    let endpoint = collector
        .endpoint
        .parse::<wispd::logs::OtlpEndpoint>()
        .unwrap();
    let logging = tokio::task::spawn_blocking(move || {
        wispd::logs::init(
            true,
            std::io::sink,
            "real-export-node",
            Some(&endpoint),
            Some(&trace_endpoint),
        )
        .unwrap()
    })
    .await
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut cfg = wispd::config::NodeConfig::dev_default();
    cfg.data_dir = directory.path().to_owned();
    cfg.engine.max_instances = 4;
    cfg.engine.max_memory_mib = 128;
    cfg.engine.guard_mib = 64;
    let node = tokio::task::spawn_blocking(move || wispd::node::Node::build(cfg).unwrap())
        .await
        .unwrap();
    let head = wisp_abi::encode(&ResponseHead {
        status: 204,
        headers: vec![],
        body: vec![],
        streaming: false,
    })
    .unwrap();
    let encoded: String = head.iter().map(|byte| format!("\\{byte:02x}")).collect();
    let guest = wat::parse_str(format!(r#"(module
        (import "wisp" "resp_new" (func $response (param i32 i32) (result i32)))
        (memory (export "memory") 2 2)
        (data (i32.const 1024) "{encoded}")
        (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "wisp_handle") (param i32 i32) (result i64)
            (i64.shl (i64.extend_i32_u (call $response (i32.const 1024) (i32.const {}))) (i64.const 32)))
        (func (export "wisp_wake") (param i64 i32 i32) (result i64) unreachable))"#, head.len())).unwrap();
    let manifest: AppManifest = toml::from_str("name = 'quotes'").unwrap();
    node.real
        .as_ref()
        .unwrap()
        .deploy_locally(
            "quotes",
            "v1",
            "acme",
            manifest,
            &guest,
            InitSandbox::Sealed,
        )
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let proxy = node.proxy.clone();
    let server = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        let service =
            hyper::service::service_fn(move |request| proxy.clone().handle(request, peer));
        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), service)
            .await
            .unwrap();
    });
    let stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let (mut client, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    let driver = tokio::spawn(connection);
    let response = client
        .send_request(
            Request::builder()
                .uri("/")
                .header("host", "quotes.wisp.dev")
                .header("connection", "close")
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "the actual sandbox response must reach its HTTP caller"
    );
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty(),
        "no-body framing must complete without payload"
    );
    driver.await.unwrap().unwrap();
    server.await.unwrap();
    tokio::task::spawn_blocking(move || {
        node.shutdown();
        drop(node);
    })
    .await
    .unwrap();
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .unwrap()
        .unwrap();
    let payload = collector.receive().await;
    let records: Vec<_> = payload
        .resource_logs
        .iter()
        .flat_map(|resource| &resource.scope_logs)
        .flat_map(|scope| &scope.log_records)
        .collect();
    assert_eq!(
        records.len(),
        1,
        "one real HTTP dispatch must export exactly one completion"
    );
    let attributes = &records[0].attributes;
    assert_eq!(
        field(attributes, "event"),
        &Value::StringValue("dispatch_completed".into()),
        "the canonical completion must cross the actual export path"
    );
    assert_eq!(
        field(attributes, "tenant"),
        &Value::StringValue("acme".into()),
        "host-established tenant ownership must survive export"
    );
    assert_eq!(
        field(attributes, "app"),
        &Value::StringValue("quotes".into()),
        "the deployed app must remain attributable"
    );
    assert_eq!(
        field(attributes, "version"),
        &Value::StringValue("v1".into()),
        "the deployed version must remain attributable"
    );
    assert_eq!(
        field(attributes, "cpu_complete"),
        &Value::BoolValue(true),
        "a normally completed measured dispatch must retain complete CPU evidence"
    );
    assert!(
        matches!(field(attributes, "cpu_ns"), Value::IntValue(_)),
        "actual measured CPU must reach OTLP numerically"
    );
    assert_eq!(
        field(attributes, "activation_cpu_included"),
        &Value::BoolValue(false),
        "the exporter must not broaden the scope of measured CPU"
    );
    let trace_payload = traces.receive().await;
    assert_eq!(
        field(
            &trace_payload.resource_spans[0]
                .resource
                .as_ref()
                .unwrap()
                .attributes,
            "service.instance.id"
        ),
        field(
            &payload.resource_logs[0]
                .resource
                .as_ref()
                .unwrap()
                .attributes,
            "service.instance.id"
        ),
        "logs and traces must identify the same process resource"
    );
    let spans: Vec<_> = trace_payload
        .resource_spans
        .iter()
        .flat_map(|resource| &resource.scope_spans)
        .flat_map(|scope| &scope.spans)
        .collect();
    let load = spans
        .iter()
        .find(|span| span.name == "template_load")
        .expect("local deployment must expose owned template loading");
    assert_eq!(
        field(&load.attributes, "mode"),
        &Value::StringValue("owned".into()),
        "local deployment must identify its owned-payload path"
    );
    for stage in [
        "template_manifest_verify",
        "template_payload_read_verify",
        "template_native_deserialize",
    ] {
        assert!(
            spans.iter().any(|span| span.name == stage
                && span.trace_id == load.trace_id
                && span.parent_span_id == load.span_id),
            "actual {stage} must nest under owned template loading"
        );
    }
    let dispatch = spans
        .iter()
        .find(|span| span.name == "dispatch")
        .expect("actual request must produce a dispatch trace");
    assert_eq!(
        records[0].trace_id, dispatch.trace_id,
        "CPU completion and stage timings must correlate to the same request"
    );
    for stage in ["acquire", "guest_slice"] {
        assert!(
            spans.iter().any(|span| span.name == stage
                && span.parent_span_id == dispatch.span_id
                && span.trace_id == dispatch.trace_id),
            "real {stage} must nest under its dispatch across the execution boundary"
        );
    }
}
