use hyper::StatusCode;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;

use super::{build, EndpointError, LogError, OtlpEndpoint};

#[path = "../../tests/common/otlp.rs"]
mod otlp;
use otlp::{field, Collector};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_stages_keep_their_dispatch_parent_across_worker_threads() {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    let mut collector = Collector::<ExportTraceServiceRequest>::start(StatusCode::OK).await;
    let endpoint = collector.endpoint.parse::<OtlpEndpoint>().unwrap();
    let (logging, dispatch) = tokio::task::spawn_blocking(move || {
        build(
            true,
            std::io::sink,
            "trace-node",
            None,
            Some(&endpoint),
            "off".parse().unwrap(),
        )
        .unwrap()
    })
    .await
    .unwrap();
    let owner = tracing::dispatcher::with_default(
        &dispatch,
        || tracing::info_span!(target: "isoplane_host::cell::completion", "dispatch", app = "todo", instance = tracing::field::Empty),
    );
    std::thread::spawn(move || {
        tracing::dispatcher::with_default(&dispatch, || {
            owner.in_scope(|| {
                let _stage =
                    tracing::info_span!(target: "isoplane_host::cell", "guest_slice").entered();
                tracing::info!(target: "isoplane_sdk", "guest payload must not become a trace event");
            });
            owner.record("instance", "instance-7");
        });
    })
    .join()
    .unwrap();
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .unwrap()
        .unwrap();
    let payload = collector.receive().await;
    let spans: Vec<_> = payload
        .resource_spans
        .iter()
        .flat_map(|resource| &resource.scope_spans)
        .flat_map(|scope| &scope.spans)
        .collect();
    assert_eq!(
        spans.len(),
        2,
        "the dispatch and its guest slice must both reach OTLP"
    );
    let parent = spans.iter().find(|span| span.name == "dispatch").unwrap();
    let child = spans
        .iter()
        .find(|span| span.name == "guest_slice")
        .unwrap();
    assert_eq!(
        child.parent_span_id, parent.span_id,
        "a resumed stage must keep its dispatch parent"
    );
    assert_eq!(
        child.trace_id, parent.trace_id,
        "moving workers must not split the request trace"
    );
    assert_eq!(
        field(&parent.attributes, "instance"),
        &Value::StringValue("instance-7".into()),
        "late instance attribution must be exported"
    );
    assert!(
        spans.iter().all(|span| span.events.is_empty()),
        "the span-only trace exporter must not include guest log events"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_fields_reach_otlp_when_the_diagnostic_log_filter_is_off() {
    let mut collector = Collector::<ExportLogsServiceRequest>::start(StatusCode::OK).await;
    let endpoint = collector.endpoint.parse::<OtlpEndpoint>().unwrap();
    let output = std::sync::Arc::new(tempfile::tempfile().unwrap());
    let writer = output.clone();
    let (logging, dispatch) = tokio::task::spawn_blocking(move || {
        build(
            true,
            writer,
            "export-node",
            Some(&endpoint),
            None,
            "off".parse().unwrap(),
        )
        .unwrap()
    })
    .await
    .unwrap();
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!(target: "isoplane_host::cell::completion", event = "dispatch_completed",
            tenant = "tenant-a", app = "app-a", version = "v7", cpu_ns = 33_u64,
            cpu_complete = false, dispatch_token = u64::MAX, "dispatch completed");
        tracing::error!(target: "unrelated", "this diagnostic must not enter the dispatch exporter");
    });
    assert!(
        output.metadata().unwrap().len() == 0,
        "the diagnostic filter must remain independently effective"
    );
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .unwrap()
        .unwrap();
    let payload = collector.receive().await;
    assert_eq!(
        payload.resource_logs.len(),
        1,
        "one node resource must attribute the exported batch"
    );
    let resource = &payload.resource_logs[0];
    assert_eq!(
        field(
            &resource.resource.as_ref().unwrap().attributes,
            "isoplane.node"
        ),
        &Value::StringValue("export-node".into()),
        "the node attribution must cross OTLP"
    );
    let records: Vec<_> = resource
        .scope_logs
        .iter()
        .flat_map(|scope| &scope.log_records)
        .collect();
    assert_eq!(
        records.len(),
        1,
        "only the structured dispatch event must be exported"
    );
    let attributes = &records[0].attributes;
    assert_eq!(
        field(attributes, "cpu_ns"),
        &Value::IntValue(33),
        "CPU usage must remain a numeric field"
    );
    assert_eq!(
        field(attributes, "cpu_complete"),
        &Value::BoolValue(false),
        "incomplete CPU evidence must not become complete in transit"
    );
    assert_eq!(
        field(attributes, "dispatch_token"),
        &Value::StringValue(u64::MAX.to_string()),
        "unsigned values beyond OTLP int64 must preserve every digit"
    );
    assert_eq!(
        field(attributes, "tenant"),
        &Value::StringValue("tenant-a".into()),
        "tenant identity must survive export"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_final_batch_is_reported_as_export_failure() {
    let mut collector = Collector::<ExportLogsServiceRequest>::start(StatusCode::BAD_REQUEST).await;
    let endpoint = collector.endpoint.parse::<OtlpEndpoint>().unwrap();
    let (logging, dispatch) = tokio::task::spawn_blocking(move || {
        build(
            true,
            std::io::sink,
            "export-node",
            Some(&endpoint),
            None,
            "off".parse().unwrap(),
        )
        .unwrap()
    })
    .await
    .unwrap();
    tracing::dispatcher::with_default(&dispatch, || {
        tracing::warn!(target: "isoplane_host::cell::completion", event = "dispatch_rejected", tenant = "tenant-a");
    });
    let result = tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .unwrap();
    let payload = collector.receive().await;
    assert!(
        matches!(result, Err(LogError::Shutdown(_))),
        "a collector rejection must not be reported as successful final export: {result:?}"
    );
    assert!(
        !payload.resource_logs.is_empty(),
        "the rejected batch must actually reach the collector"
    );
}

#[test]
fn telemetry_endpoints_refuse_embedded_secret_material() {
    for endpoint in [
        "https://user:secret@localhost/v1/logs",
        "https://localhost/v1/logs?token=secret",
        "https://localhost/v1/logs#secret",
    ] {
        let result = endpoint.parse::<OtlpEndpoint>();
        assert!(
            matches!(result, Err(EndpointError::EmbeddedCredentials)),
            "endpoint configuration must not carry secrets into printed config"
        );
        assert!(
            !result.unwrap_err().to_string().contains("secret@"),
            "endpoint failures must not echo credentials"
        );
    }
}

#[test]
fn telemetry_endpoints_require_an_absolute_http_destination() {
    assert!(
        matches!(
            "/v1/logs".parse::<OtlpEndpoint>(),
            Err(EndpointError::Url(_))
        ),
        "a relative endpoint must fail before exporter construction"
    );
    assert!(
        matches!(
            "file:///tmp/logs".parse::<OtlpEndpoint>(),
            Err(EndpointError::Scheme)
        ),
        "a non-HTTP destination must fail before exporter construction"
    );
}
