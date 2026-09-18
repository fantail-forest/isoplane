use tracing::{field::Visit, Event, Metadata, Subscriber};
use tracing_subscriber::{layer::Context, Layer};

#[derive(Clone, Copy)]
enum CollectorDiagnostic {
    Status,
    PartialSuccess,
    TracePartialSuccess,
}

impl CollectorDiagnostic {
    fn from_metadata(metadata: &Metadata<'_>) -> Option<Self> {
        if metadata.target() != "opentelemetry-otlp" {
            return None;
        }
        match metadata.name() {
            "HttpClient.StatusError" => Some(Self::Status),
            "HttpLogsClient.PartialSuccess" => Some(Self::PartialSuccess),
            "HttpTraceClient.PartialSuccess" => Some(Self::TracePartialSuccess),
            _ => None,
        }
    }
}

pub(super) fn contains_no_collector_payload(metadata: &Metadata<'_>) -> bool {
    CollectorDiagnostic::from_metadata(metadata).is_none()
}

pub(super) struct CollectorDiagnostics;

#[derive(Default)]
struct SafeFields {
    status_code: Option<u64>,
    rejected_log_records: Option<i64>,
    rejected_spans: Option<i64>,
}

impl Visit for SafeFields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "status_code" {
            self.status_code = Some(value);
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if field.name() == "rejected_log_records" {
            self.rejected_log_records = Some(value);
        }
        if field.name() == "rejected_spans" {
            self.rejected_spans = Some(value);
        }
    }

    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {
        // Collector text is intentionally never formatted, copied, or retained.
    }
}

impl<S: Subscriber> Layer<S> for CollectorDiagnostics {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let Some(diagnostic) = CollectorDiagnostic::from_metadata(event.metadata()) else {
            return;
        };
        let mut fields = SafeFields::default();
        event.record(&mut fields);
        match diagnostic {
            CollectorDiagnostic::Status => tracing::warn!(
                event = "otlp_http_rejected",
                status_code = fields.status_code,
                "OTLP collector rejected the HTTP export"
            ),
            CollectorDiagnostic::PartialSuccess => tracing::warn!(
                event = "otlp_partial_success",
                rejected_log_records = fields.rejected_log_records,
                "OTLP collector reported partial success; collector message withheld"
            ),
            CollectorDiagnostic::TracePartialSuccess => tracing::warn!(
                event = "otlp_partial_success",
                rejected_spans = fields.rejected_spans,
                "OTLP collector rejected spans; collector message withheld"
            ),
        }
    }
}
