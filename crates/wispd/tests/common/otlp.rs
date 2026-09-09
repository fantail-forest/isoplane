use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::{any_value::Value, KeyValue};
use prost::Message;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct Collector {
    pub endpoint: String,
    received: mpsc::Receiver<ExportLogsServiceRequest>,
    task: tokio::task::JoinHandle<()>,
}

impl Collector {
    pub async fn start(status: StatusCode) -> Self {
        Self::with_response(status, ExportLogsServiceResponse::default().encode_to_vec()).await
    }

    pub async fn with_response(status: StatusCode, response_body: Vec<u8>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/logs", listener.local_addr().unwrap());
        let (sender, received) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service =
                hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                    let sender = sender.clone();
                    let response_body = response_body.clone();
                    async move {
                        assert_eq!(request.method(), "POST", "OTLP export must use POST");
                        assert_eq!(
                            request.uri().path(),
                            "/v1/logs",
                            "the supplied signal endpoint must be used exactly"
                        );
                        assert_eq!(
                            request.headers()["content-type"],
                            "application/x-protobuf",
                            "the payload must declare the OTLP binary format"
                        );
                        let body = request.into_body().collect().await.unwrap().to_bytes();
                        let decoded = ExportLogsServiceRequest::decode(body)
                            .expect("the maintained OTLP codec must decode the exporter payload");
                        sender
                            .send(decoded)
                            .await
                            .expect("the test must retain its collector receiver");
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(status)
                                .header("connection", "close")
                                .header("content-type", "application/x-protobuf")
                                .body(Full::new(Bytes::from(response_body)))
                                .unwrap(),
                        )
                    }
                });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        });
        Self {
            endpoint,
            received,
            task,
        }
    }

    pub async fn receive(&mut self) -> ExportLogsServiceRequest {
        tokio::time::timeout(Duration::from_secs(30), self.received.recv())
            .await
            .expect("OTLP export must not hang")
            .expect("the exporter must send its queued events")
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn field<'a>(attributes: &'a [KeyValue], name: &str) -> &'a Value {
    attributes
        .iter()
        .find(|attribute| attribute.key == name)
        .and_then(|attribute| attribute.value.as_ref())
        .and_then(|value| value.value.as_ref())
        .unwrap_or_else(|| panic!("missing OTLP attribute {name}"))
}

/// Subscriber output shared by provider and collector diagnostic tests.
#[allow(
    dead_code,
    reason = "Each test binary includes this shared module but exercises a different logging boundary."
)]
pub mod output {
    /// Shared bytes emitted by the node subscriber under test.
    #[derive(Clone, Default)]
    pub struct LogOutput(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl LogOutput {
        /// Reads complete formatted events after the writer releases its event guard.
        pub fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("test log writer").clone())
                .expect("tracing emits UTF-8")
        }
    }

    impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for LogOutput {
        type Writer = tracing_subscriber::fmt::writer::MutexGuardWriter<'writer, Vec<u8>>;

        fn make_writer(&'writer self) -> Self::Writer {
            tracing_subscriber::fmt::MakeWriter::make_writer(self.0.as_ref())
        }
    }

    /// One guest event decoded from the node's actual JSON logging output.
    #[derive(Debug, serde::Deserialize)]
    pub struct EmittedLog {
        /// Position among the actual guest events in this test writer.
        #[serde(skip)]
        pub emission_index: usize,
        /// Host-established tenant.
        pub tenant: String,
        /// Host-established module and version.
        pub module: String,
        /// Host-established instance identifier.
        pub instance: String,
        /// Host-established dispatch identifier.
        pub dispatch: u64,
        /// ABI log severity.
        pub level: u8,
        /// Untrusted guest message text.
        pub fields: String,
    }

    /// Reads the guest events emitted through the node subscriber's writer.
    pub fn guest_logs(output: &LogOutput) -> Vec<EmittedLog> {
        output
            .text()
            .lines()
            .filter_map(|line| {
                let event: serde_json::Value =
                    serde_json::from_str(line).expect("tracing JSON event");
                if event["fields"]["message"] != "guest log" {
                    return None;
                }
                Some(
                    serde_json::from_value::<EmittedLog>(event["fields"].clone())
                        .expect("the node emits complete guest attribution"),
                )
            })
            .enumerate()
            .map(|(emission_index, mut record)| {
                record.emission_index = emission_index;
                record
            })
            .collect()
    }
}
