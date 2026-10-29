use hyper::StatusCode;
use isoplane_test_support::{assert_success, ChildTest};
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsPartialSuccess, ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use prost::Message;

#[path = "common/otlp.rs"]
pub mod otlp;

const SENTINEL: &str = "collector-echoed-authorization-secret-9c47";
const CHILD: ChildTest =
    ChildTest::new("collector_payloads_never_reach_diagnostics_even_with_verbose_logging");

#[test]
fn collector_payloads_never_reach_diagnostics_even_with_verbose_logging() {
    if let Some(case) = CHILD.child_value() {
        let case = case.to_str().expect("the parent passes a case name");
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(exercise_collector(case));
        return;
    }
    for level in ["debug", "trace"] {
        for case in [
            "status",
            "partial",
            "healthy",
            "client_error",
            "trace_partial",
            "trace_error",
        ] {
            let output = CHILD.command(case).env("RUST_LOG", level).output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stdout.contains(SENTINEL) && !stderr.contains(SENTINEL),
                "collector response text escaped through raw diagnostics at {level} for {case}"
            );
            assert_success(
                &output,
                &format!("the global exporter diagnostic child for {case}/{level}"),
            );
        }
    }
}

async fn exercise_collector(case: &str) {
    if case.starts_with("trace_") {
        exercise_trace_collector(case == "trace_partial").await;
        return;
    }
    let partial = case == "partial";
    let mut collector = if case == "healthy" {
        otlp::Collector::<ExportLogsServiceRequest>::start(StatusCode::OK).await
    } else if partial {
        otlp::Collector::<ExportLogsServiceRequest>::with_response(
            StatusCode::OK,
            ExportLogsServiceResponse {
                partial_success: Some(ExportLogsPartialSuccess {
                    rejected_log_records: 7,
                    error_message: SENTINEL.into(),
                }),
            }
            .encode_to_vec(),
        )
        .await
    } else {
        let status = if case == "client_error" {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::FOUND
        };
        otlp::Collector::<ExportLogsServiceRequest>::with_response(
            status,
            SENTINEL.as_bytes().to_vec(),
        )
        .await
    };
    let endpoint = collector
        .endpoint
        .parse::<isoplaned::logs::OtlpEndpoint>()
        .unwrap();
    let output = otlp::output::LogOutput::default();
    let writer =
        tracing_subscriber::fmt::writer::MakeWriterExt::and(output.clone(), std::io::stderr);
    let logging = tokio::task::spawn_blocking(move || {
        isoplaned::logs::init(true, writer, "diagnostic-test", Some(&endpoint), None).unwrap()
    })
    .await
    .unwrap();
    tracing::info!(target: "isoplane_host::cell::completion", event = "dispatch_completed",
        tenant = "test-tenant", app = "test-app", version = "test-version",
        "dispatch completed");
    let shutdown = tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .unwrap();
    assert_eq!(
        shutdown.is_ok(),
        matches!(case, "partial" | "healthy"),
        "HTTP rejection must retain the SDK flush failure; partial success follows the SDK result"
    );
    let exported = collector.receive().await;
    let attributes = &exported.resource_logs[0].scope_logs[0].log_records[0].attributes;
    assert_eq!(
        otlp::field(attributes, "tenant"),
        &Value::StringValue("test-tenant".into()),
        "the real collector must receive the triggering export"
    );
    let lines: Vec<_> = output.text().lines().map(str::to_owned).collect();
    assert!(
        lines.iter().all(|line| !line.contains(SENTINEL)),
        "collector text must never enter the formatted diagnostic output"
    );
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    if case == "healthy" {
        assert!(
            records
                .iter()
                .all(|record| record["fields"]["event"] != "otlp_http_rejected"
                    && record["fields"]["event"] != "otlp_partial_success"),
            "a fully accepted export must not fabricate collector rejection"
        );
        return;
    }
    if case == "client_error" {
        assert!(records.iter().any(|record| record["target"] == "opentelemetry_sdk" && record["level"] == "ERROR"),
            "reqwest HTTP client rejection must retain the SDK failure");
        return;
    }
    let (event, field, count) = if partial {
        ("otlp_partial_success", "rejected_log_records", 7)
    } else {
        ("otlp_http_rejected", "status_code", 302)
    };
    assert!(
        records
            .iter()
            .any(|record| record["fields"]["event"] == event && record["fields"][field] == count),
        "sanitized numeric collector outcome must remain visible: {lines:?}"
    );
    if !partial {
        assert!(
            records.iter().any(|record| record["target"] == "opentelemetry_sdk"
                && record["level"] == "ERROR"),
            "SDK export failure must remain visible alongside the safe HTTP status: {lines:?}"
        );
    }
    println!("formatted_logs={}", serde_json::to_string(&lines).unwrap());
}

async fn exercise_trace_collector(partial: bool) {
    use opentelemetry_proto::tonic::collector::trace::v1::{
        ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
    };
    let (status, response) = if partial {
        (
            StatusCode::OK,
            ExportTraceServiceResponse {
                partial_success: Some(ExportTracePartialSuccess {
                    rejected_spans: 7,
                    error_message: SENTINEL.into(),
                }),
            }
            .encode_to_vec(),
        )
    } else {
        (StatusCode::BAD_REQUEST, SENTINEL.as_bytes().to_vec())
    };
    let mut collector =
        otlp::Collector::<ExportTraceServiceRequest>::with_response(status, response).await;
    let endpoint = collector
        .endpoint
        .parse::<isoplaned::logs::OtlpEndpoint>()
        .unwrap();
    let output = otlp::output::LogOutput::default();
    let writer =
        tracing_subscriber::fmt::writer::MakeWriterExt::and(output.clone(), std::io::stderr);
    let logging = tokio::task::spawn_blocking(move || {
        isoplaned::logs::init(true, writer, "trace-diagnostic-test", None, Some(&endpoint)).unwrap()
    })
    .await
    .unwrap();
    tracing::info_span!(target: "isoplane_host::cell", "guest_slice").in_scope(|| {});
    let shutdown = tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .unwrap();
    assert_eq!(
        shutdown.is_ok(),
        partial,
        "a rejected trace batch must retain its flush failure"
    );
    let exported = collector.receive().await;
    assert_eq!(
        exported.resource_spans[0].scope_spans[0].spans[0].name, "guest_slice",
        "the trace collector must receive the triggering span"
    );
    let lines: Vec<_> = output.text().lines().map(str::to_owned).collect();
    assert!(
        lines.iter().all(|line| !line.contains(SENTINEL)),
        "trace collector response text must not enter the formatted diagnostic output"
    );
    if partial {
        assert!(
            lines.iter().any(|line| {
                let record: serde_json::Value = serde_json::from_str(line).unwrap();
                record["fields"]["event"] == "otlp_partial_success"
                    && record["fields"]["rejected_spans"] == 7
            }),
            "partial trace acceptance must report the rejected count without collector text"
        );
    }
}
