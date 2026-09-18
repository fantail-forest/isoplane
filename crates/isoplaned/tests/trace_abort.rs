use hyper::StatusCode;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;

#[path = "common/otlp.rs"]
mod otlp;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminating_a_process_drops_its_unfinished_trace() {
    const CHILD_ENDPOINT: &str = "ISOPLANE_TRACE_ABORT_TEST_ENDPOINT";
    if let Ok(endpoint) = std::env::var(CHILD_ENDPOINT) {
        let endpoint = endpoint.parse().expect("test collector endpoint");
        let _logging = tokio::task::spawn_blocking(move || {
            isoplaned::logs::init(true, std::io::sink, "abort-test", None, Some(&endpoint))
                .expect("trace exporter")
        })
        .await
        .expect("logging task");
        let unfinished = tracing::info_span!(target: "isoplane_test", "unfinished");
        unfinished.in_scope(|| {
            tracing::info_span!(target: "isoplane_test", "ended_child").in_scope(|| {});
        });
        tracing::info_span!(target: "isoplane_test", "completed_barrier", marker = "ready")
            .in_scope(|| {});
        std::future::pending::<()>().await;
        return;
    }
    let mut collector = otlp::Collector::<ExportTraceServiceRequest>::start(StatusCode::OK).await;
    let mut child = tokio::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "terminating_a_process_drops_its_unfinished_trace",
            "--nocapture",
        ])
        .env(CHILD_ENDPOINT, &collector.endpoint)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("child process");
    let payload = collector.receive().await;
    child.start_kill().expect("terminate test child");
    let status = child.wait().await.expect("reap child");
    assert!(
        !status.success(),
        "the test must exercise forced termination"
    );
    let spans: Vec<_> = payload
        .resource_spans
        .iter()
        .flat_map(|resource| &resource.scope_spans)
        .flat_map(|scope| &scope.spans)
        .collect();
    assert_eq!(
        spans.len(),
        1,
        "the ended child of an unfinished trace must not precede the completed barrier"
    );
    assert_eq!(
        spans[0].name, "completed_barrier",
        "only the completed trace may reach the real collector"
    );
    assert_eq!(
        otlp::field(&spans[0].attributes, "marker"),
        &Value::StringValue("ready".into()),
        "the observed trace must be the child process's completion barrier"
    );
}
