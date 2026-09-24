use super::*;
use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, StreamBody};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

#[derive(Default)]
struct Fields(BTreeMap<String, String>);
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
}

struct Closed(mpsc::UnboundedSender<Fields>);
impl<S: tracing::Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Closed {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: LayerContext<'_, S>,
    ) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        ctx.span(id)
            .expect("new span")
            .extensions_mut()
            .insert(fields);
    }
    fn on_record(
        &self,
        id: &tracing::Id,
        values: &tracing::span::Record<'_>,
        ctx: LayerContext<'_, S>,
    ) {
        let span = ctx.span(id).expect("recorded span");
        let mut extensions = span.extensions_mut();
        values.record(extensions.get_mut::<Fields>().expect("span fields"));
    }
    fn on_close(&self, id: tracing::Id, ctx: LayerContext<'_, S>) {
        let span = ctx.span(&id).expect("closing span");
        let fields = span
            .extensions_mut()
            .remove::<Fields>()
            .expect("closing fields");
        self.0.send(fields).expect("completion observer retained");
    }
}

#[derive(Clone, Copy)]
enum End {
    Head,
    Complete,
    Cancel,
    Error,
}

async fn observe(end: End) -> Fields {
    tokio::time::timeout(Duration::from_secs(10), observe_body(end))
        .await
        .expect("the real HTTP body scenario must not hang")
}

async fn observe_body(end: End) -> Fields {
    let (closed, mut observed) = mpsc::unbounded_channel();
    let subscriber = tracing_subscriber::registry().with(Closed(closed));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let (send, receive) = mpsc::channel::<Result<Frame<Bytes>, std::io::Error>>(2);
    let receive = std::sync::Mutex::new(Some(receive));
    let serving =
        tokio::spawn(
            async move {
                let (stream, _) = listener.accept().await.expect("request connection");
                let service =
                    service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                        let receiver = receive
                            .lock()
                            .expect("receiver lock")
                            .take()
                            .expect("one request");
                        let trace = RequestTrace::new(&request);
                        let body = StreamBody::new(futures::stream::unfold(
                            receiver,
                            |mut receiver| async move {
                                receiver.recv().await.map(|frame| (frame, receiver))
                            },
                        ))
                        .boxed_unsync();
                        async move {
                            Ok::<_, std::convert::Infallible>(trace.response(Response::new(body)))
                        }
                    });
                let _result = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            }
            .with_subscriber(subscriber),
        );
    let stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("client");
    let (mut client, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("handshake");
    let connection = tokio::spawn(connection);
    send.send(Ok(Frame::data(Bytes::from_static(b"first"))))
        .await
        .expect("first frame");
    let request = hyper::Request::builder()
        .method(if matches!(end, End::Head) {
            "HEAD"
        } else {
            "GET"
        })
        .uri("/")
        .body(Empty::<Bytes>::new())
        .expect("request");
    let mut body = client
        .send_request(request)
        .await
        .expect("response headers")
        .into_body();
    if !matches!(end, End::Head) {
        assert_eq!(
            body.frame()
                .await
                .expect("first frame")
                .expect("frame")
                .into_data()
                .expect("data"),
            "first",
            "the first streamed frame must precede completion"
        );
        assert!(
            observed.try_recv().is_err(),
            "response headers and partial bytes cannot close the request trace"
        );
    }
    match end {
        End::Head => {
            assert!(
                body.collect()
                    .await
                    .expect("HEAD response")
                    .to_bytes()
                    .is_empty(),
                "Hyper must suppress even a nonempty application body for HEAD"
            );
            drop(send);
        }
        End::Complete => {
            drop(send);
            assert!(
                body.collect()
                    .await
                    .expect("completed response")
                    .to_bytes()
                    .is_empty(),
                "EOF must follow the first frame"
            );
        }
        End::Cancel => {
            drop(body);
            connection.abort();
        }
        End::Error => {
            send.send(Err(std::io::Error::other("test body failure")))
                .await
                .expect("error frame");
            assert!(
                body.collect().await.is_err(),
                "body failure must reach the HTTP client"
            );
        }
    }
    drop(client);
    let fields = tokio::time::timeout(Duration::from_secs(10), observed.recv())
        .await
        .expect("completion deadline")
        .expect("closed request trace");
    connection.abort();
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("server shutdown deadline")
        .expect("server task");
    assert_ne!(
        fields.0["request_id"], "ffffffffffffffffffffffffffffffff",
        "public ingress must replace forged correlation"
    );
    assert_eq!(
        fields.0["status"], "200",
        "body outcome must retain response status"
    );
    fields
}

#[tokio::test]
async fn a_request_trace_ends_only_after_streamed_body_eof() {
    assert_eq!(
        observe(End::Complete).await.0["outcome"],
        "complete",
        "body EOF must complete its request trace"
    );
}

#[tokio::test]
async fn a_cancelled_http_body_closes_its_request_trace_as_cancelled() {
    assert_eq!(
        observe(End::Cancel).await.0["outcome"],
        "cancelled",
        "client cancellation must retain cancellation attribution"
    );
}

#[tokio::test]
async fn a_failed_http_body_closes_its_request_trace_as_body_error() {
    assert_eq!(
        observe(End::Error).await.0["outcome"],
        "body_error",
        "body errors cannot be relabelled as successful EOF"
    );
}

#[tokio::test]
async fn a_head_response_completes_without_polling_its_nonempty_body() {
    assert_eq!(
        observe(End::Head).await.0["outcome"],
        "complete",
        "HTTP body suppression is successful completion, not cancellation"
    );
}
