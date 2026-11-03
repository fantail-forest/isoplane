use hyper::StatusCode;

use isoplane_abi::result::ResultRecord;
use isoplane_host::futures::{FutureTable, WakeTarget};
use isoplane_host::InstanceId;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;

#[path = "common/otlp.rs"]
mod otlp;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_future_waits_export_one_terminal_outcome() {
    let mut collector = otlp::Collector::<ExportTraceServiceRequest>::start(StatusCode::OK).await;
    let endpoint = collector.endpoint.parse().expect("collector endpoint");
    let logging = tokio::task::spawn_blocking(move || {
        isoplaned::logs::init(true, std::io::sink, "future-waits", None, Some(&endpoint))
            .expect("trace exporter")
    })
    .await
    .expect("logging task");
    for outcome in ["ready", "cancelled", "discarded"] {
        tracing::info_span!(target: "isoplane_test", "operation", outcome).in_scope(|| {
            let table = FutureTable::new(1024);
            let owner = InstanceId(1);
            let id = table
                .alloc(owner, 1, "test_operation", 4)
                .expect("pending host operation");
            table.alloc_cancellation(owner, 1);
            match outcome {
                "ready" => assert_eq!(
                    table.complete(id, ResultRecord::Ok(vec![])),
                    WakeTarget::Instance(owner),
                    "completion must make the operation deliverable"
                ),
                "cancelled" => assert!(
                    table.cancel_owned(owner, id.into()),
                    "cancellation must remove the owned operation"
                ),
                "discarded" => drop(table),
                _ => unreachable!("the case list defines every terminal outcome"),
            }
        });
    }
    tokio::task::spawn_blocking(move || logging.shutdown())
        .await
        .expect("shutdown task")
        .expect("flush complete traces");
    let payload = collector.receive().await;
    let spans: Vec<_> = payload
        .resource_spans
        .iter()
        .flat_map(|resource| &resource.scope_spans)
        .flat_map(|scope| &scope.spans)
        .collect();
    let waits: Vec<_> = spans
        .iter()
        .filter(|span| span.name == "host_future_wait")
        .collect();
    assert_eq!(
        waits.len(),
        3,
        "only the three host operations, not implicit cancellation futures, represent waits"
    );
    for waiting in waits {
        let parent = spans
            .iter()
            .find(|span| span.span_id == waiting.parent_span_id)
            .expect("the pending operation must retain its initiating context");
        assert_eq!(
            otlp::field(&waiting.attributes, "outcome"),
            otlp::field(&parent.attributes, "outcome"),
            "each terminal path must close its wait with the actual outcome"
        );
        assert_eq!(
            waiting
                .attributes
                .iter()
                .filter(|field| field.key == "outcome")
                .count(),
            1,
            "the exported outcome must not contain a conflicting provisional value"
        );
        assert_eq!(
            otlp::field(&waiting.attributes, "import"),
            &Value::StringValue("test_operation".into()),
            "the wait must preserve its capability identity"
        );
    }
}
