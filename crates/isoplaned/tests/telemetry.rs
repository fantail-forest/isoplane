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
    use isoplane_abi::config::{AppManifest, InitSandbox};
    use isoplane_abi::dispatch::ResponseHead;

    let mut collector = Collector::<ExportLogsServiceRequest>::start(StatusCode::OK).await;
    let mut traces = Collector::<ExportTraceServiceRequest>::start(StatusCode::OK).await;
    let trace_endpoint = traces
        .endpoint
        .parse::<isoplaned::logs::OtlpEndpoint>()
        .unwrap();
    let endpoint = collector
        .endpoint
        .parse::<isoplaned::logs::OtlpEndpoint>()
        .unwrap();
    let logging = tokio::task::spawn_blocking(move || {
        isoplaned::logs::init(
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
    let mut cfg = isoplaned::config::NodeConfig::dev_default();
    cfg.data_dir = directory.path().to_owned();
    cfg.engine.max_instances = 4;
    cfg.engine.memory_limit_mib = 128;
    cfg.engine.guard_mib = 64;
    let node = tokio::task::spawn_blocking(move || isoplaned::node::Node::build(cfg).unwrap())
        .await
        .unwrap();
    let head = isoplane_abi::encode(&ResponseHead {
        status: 204,
        headers: vec![],
        body: vec![],
        streaming: false,
    })
    .unwrap();
    let encoded = isoplane_test_support::wat_bytes(&head);
    let guest = wat::parse_str(format!(r#"(module
        (import "isoplane" "resp_new" (func $response (param i32 i32) (result i32)))
        (import "isoplane" "clock_deadline_in" (func $timer (param i64) (result i64)))
        (memory (export "memory") 2 2)
        (data (i32.const 1024) "{encoded}")
        (func (export "isoplane_alloc") (param i32 i32) (result i32) (i32.const 4096))
        (func (export "isoplane_handle") (param i32 i32) (result i64)
            (drop (call $timer (i64.const 0))) (i64.const 1))
        (func (export "isoplane_wake") (param i64 i32 i32) (result i64)
            (i64.shl (i64.extend_i32_u (call $response (i32.const 1024) (i32.const {}))) (i64.const 32)))
        )"#, head.len())).unwrap();
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
                .header("host", "quotes.isoplane.dev")
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
        let cell = node
            .real
            .as_ref()
            .unwrap()
            .cells()
            .iter()
            .find(|cell| !cell.resident_ids().is_empty())
            .expect("resident HTTP instance");
        let instance = cell.resident_ids()[0];
        cell.force_hibernate(instance)
            .expect("capture idle HTTP instance");
        assert_eq!(
            cell.instance_state(instance),
            Some(isoplane_host::LifecycleTier::Hibernated),
            "the trace must come from an actual hibernated instance"
        );
        cell.force_wake(instance).expect("restore HTTP instance");
        assert_eq!(
            cell.instance_state(instance),
            Some(isoplane_host::LifecycleTier::Parked),
            "the trace must come from an actual restored instance"
        );
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
    let mut records: Vec<_> = payload
        .resource_logs
        .iter()
        .flat_map(|resource| &resource.scope_logs)
        .flat_map(|scope| scope.log_records.iter().cloned())
        .collect();
    while records.len() < 3 {
        let payload = collector.receive().await;
        records.extend(
            payload
                .resource_logs
                .into_iter()
                .flat_map(|resource| resource.scope_logs)
                .flat_map(|scope| scope.log_records),
        );
    }
    assert_eq!(
        records
            .iter()
            .filter(|record| field(&record.attributes, "event")
                == &Value::StringValue("dispatch_completed".into()))
            .count(),
        1,
        "one real HTTP dispatch must export exactly one completion"
    );
    let completion = records
        .iter()
        .find(|record| {
            field(&record.attributes, "event") == &Value::StringValue("dispatch_completed".into())
        })
        .expect("dispatch completion exported");
    let attributes = &completion.attributes;
    for (previous, current, cause) in [
        ("parked", "hibernated", "capture"),
        ("hibernated", "parked", "restore"),
    ] {
        let transitions: Vec<_> = records
            .iter()
            .filter(|record| {
                field(&record.attributes, "event")
                    == &Value::StringValue("instance_transition".into())
                    && field(&record.attributes, "current") == &Value::StringValue(current.into())
            })
            .collect();
        assert_eq!(
            transitions.len(),
            1,
            "each actual lifecycle transition must export once"
        );
        let fields = &transitions[0].attributes;
        for (key, expected) in [
            ("previous", previous),
            ("cause", cause),
            ("tenant", "acme"),
            ("app", "quotes"),
            ("version", "v1"),
        ] {
            assert_eq!(
                field(fields, key),
                &Value::StringValue(expected.into()),
                "lifecycle export must preserve {key}"
            );
        }
        assert_eq!(
            field(fields, "instance"),
            field(attributes, "instance"),
            "transition and dispatch must identify the same guest"
        );
    }
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
        field(attributes, "occupancy_complete"),
        &Value::BoolValue(true),
        "a normally completed measured dispatch must retain complete occupancy evidence"
    );
    assert!(
        matches!(field(attributes, "occupancy_ns"), Value::IntValue(_)),
        "actual measured occupancy must reach OTLP numerically"
    );
    assert_eq!(
        field(attributes, "activation_occupancy_included"),
        &Value::BoolValue(false),
        "the exporter must not broaden the scope of measured occupancy"
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
        .expect("local deployment must expose template loading");
    for stage in [
        "template_manifest_verify",
        "template_payload_verify",
        "template_read_ahead",
        "template_native_deserialize",
    ] {
        assert!(
            spans.iter().any(|span| span.name == stage
                && span.trace_id == load.trace_id
                && span.parent_span_id == load.span_id),
            "actual {stage} must nest under template loading"
        );
    }
    let dispatch = spans
        .iter()
        .find(|span| span.name == "dispatch")
        .expect("actual request must produce a dispatch trace");
    let wake = spans
        .iter()
        .find(|span| span.name == "wake_queue")
        .expect("an actual timer completion must expose wake queue residency");
    let waiting = spans
        .iter()
        .find(|span| span.name == "host_future_wait" && span.trace_id == dispatch.trace_id)
        .expect("the actual host timer must expose its pending lifetime");
    assert_eq!(
        field(&waiting.attributes, "import"),
        &Value::StringValue("clock_deadline_in".into()),
        "the wait must identify the capability that created it"
    );
    assert_eq!(
        field(&waiting.attributes, "outcome"),
        &Value::StringValue("ready".into()),
        "a completed timer must finish its wait before delivery"
    );
    for attribute in ["tenant", "app", "version", "dispatch_id"] {
        assert_eq!(
            field(&wake.attributes, attribute),
            field(&dispatch.attributes, attribute),
            "wake queue work must correlate to its dispatch by {attribute}"
        );
    }
    assert_eq!(
        completion.trace_id, dispatch.trace_id,
        "occupancy completion and stage timings must correlate to the same request"
    );
    let activation = spans
        .iter()
        .find(|span| span.name == "activation" && span.trace_id == dispatch.trace_id)
        .expect("first request must expose actual activation work");
    assert_eq!(
        field(&activation.attributes, "linear_memory_bytes"),
        &Value::StringValue((2 * 65536).to_string()),
        "activation must report the fixture's actual linear memory extent"
    );
    for stage in [
        "activation_store",
        "activation_instantiate",
        "activation_exports",
        "activation_table_baseline",
    ] {
        assert!(
            spans.iter().any(|span| span.name == stage
                && span.trace_id == activation.trace_id
                && span.parent_span_id == activation.span_id),
            "actual {stage} must nest under the request's activation"
        );
    }
    for operation in ["capture", "restore"] {
        let span = spans
            .iter()
            .find(|span| span.name == operation)
            .expect("idle lifecycle operation must remain observable without a live dispatch");
        assert_eq!(
            field(&span.attributes, "app"),
            &Value::StringValue("quotes".into()),
            "idle lifecycle work must retain application ownership"
        );
    }
    for stage in [
        "delta_diff",
        "delta_write",
        "delta_store_load",
        "restore_activate",
        "delta_apply",
    ] {
        assert!(
            spans.iter().any(|span| span.name == stage),
            "real lifecycle must expose {stage}"
        );
    }
    for stage in ["dispatch_queue", "acquire", "guest_slice"] {
        assert!(
            spans.iter().any(|span| span.name == stage
                && span.parent_span_id == dispatch.span_id
                && span.trace_id == dispatch.trace_id),
            "real {stage} must nest under its dispatch across the execution boundary"
        );
    }
}
