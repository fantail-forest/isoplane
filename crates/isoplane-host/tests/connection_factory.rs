//! HTTP uses the same injected socket origin as ordinary managed TCP.
#[path = "common/connection_factory.rs"]
mod connection_factory;

use connection_factory::RecordingConnections;
use isoplane_host::bindings::{Fetcher, OutboundBody, ResponseBody};
use isoplane_host::egress::{
    ConnectAttempt, ConnectionOwner, DenyReason, DestinationPolicy, DnsError, HostEgressPolicy,
    Resolver,
};
use isoplane_host::{InstanceId, PinnedHttpFetcher};
use std::net::SocketAddr;
use std::sync::Arc;

struct NoDns;
#[async_trait::async_trait]
impl Resolver for NoDns {
    async fn resolve(&self, _: &str, _: u16) -> Result<Vec<SocketAddr>, DnsError> {
        panic!("a checked numeric endpoint must not resolve DNS again")
    }
}
struct ExactPeer(SocketAddr);
impl DestinationPolicy for ExactPeer {
    fn authorize(&self, address: SocketAddr) -> Result<(), DenyReason> {
        if address == self.0 {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(address.to_string()))
        }
    }
}
fn request() -> isoplane_abi::calls::FetchRequest {
    isoplane_abi::calls::FetchRequest {
        method: "GET".into(),
        url: "http://owned.test/".into(),
        headers: vec![],
        body: vec![],
    }
}
fn target(address: SocketAddr) -> isoplane_abi::host::PinnedTarget {
    isoplane_abi::host::PinnedTarget {
        url: "http://owned.test/".into(),
        addr: address,
    }
}
fn owner() -> ConnectionOwner {
    ConnectionOwner::new("factory-tenant".into(), InstanceId(91))
        .expect("the test has an explicit tenant")
}
async fn attempt(policy: &Arc<HostEgressPolicy>) -> ConnectAttempt {
    policy
        .begin_connect(owner())
        .await
        .expect("the instance may start a connection")
}

#[tokio::test]
async fn http_uses_the_injected_connection_provider_with_dispatch_identity() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the owned HTTP server must bind");
    let address = listener
        .local_addr()
        .expect("the owned server must have an endpoint");
    let server = tokio::spawn(async move {
        let (stream, _) = listener
            .accept()
            .await
            .expect("HTTP must reach the owned server");
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                hyper_util::rt::TokioIo::new(stream),
                hyper::service::service_fn(|_| async {
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                        http_body_util::Full::new(bytes::Bytes::from_static(b"provider")),
                    ))
                }),
            )
            .await
            .expect("the owned HTTP exchange must complete");
    });
    let connections = Arc::new(RecordingConnections::default());
    let policy = Arc::new(
        HostEgressPolicy::new(
            Box::new(NoDns),
            isoplane_host::egress::ConnectionLimits::uniform(1).expect("connection limits"),
        )
        .with_destinations(Arc::new(ExactPeer(address)))
        .with_connections(connections.clone()),
    );
    let fetcher = PinnedHttpFetcher::new(1024);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        fetcher.fetch(attempt(&policy).await, target(address), request()),
    )
    .await
    .expect("the owned HTTP request must finish before its test deadline")
    .expect("HTTP must use the working provider");
    let OutboundBody::Inline(body) = response.body else {
        panic!("a small response must be inline")
    };
    assert_eq!(
        body, b"provider",
        "the injected connection must carry the real HTTP response"
    );
    assert_eq!(
        *connections.calls.lock(),
        vec![(owner(), address)],
        "HTTP must pass its actual tenant, instance, and pinned endpoint to the shared provider"
    );
    server.await.expect("the owned HTTP server must not panic");
}

#[tokio::test]
async fn http_refuses_a_connection_provider_that_returns_a_different_peer() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the wrong peer must bind");
    let wrong = listener
        .local_addr()
        .expect("the wrong peer must have an endpoint");
    let expected = SocketAddr::from(([93, 184, 216, 34], 80));
    let connections = Arc::new(RecordingConnections {
        substitute: Some(wrong),
        ..Default::default()
    });
    let policy = Arc::new(
        HostEgressPolicy::new(
            Box::new(NoDns),
            isoplane_host::egress::ConnectionLimits::uniform(1).expect("connection limits"),
        )
        .with_connections(connections),
    );
    let fetcher = PinnedHttpFetcher::new(1024);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        fetcher.fetch(attempt(&policy).await, target(expected), request()),
    )
    .await
    .expect("a substituted connection must be refused before the test deadline");
    assert!(
        matches!(result, Err(isoplane_abi::result::FutureError::Io(_))),
        "a provider cannot substitute an unchecked HTTP peer"
    );
    let (mut peer, _) = listener
        .accept()
        .await
        .expect("the deliberately wrong connection must exist");
    let mut byte = [0];
    assert_eq!(
        peer.read(&mut byte)
            .await
            .expect("the refused peer must observe closure"),
        0,
        "the HTTP transport must send no request bytes to a substituted destination"
    );
}

#[tokio::test]
async fn http_denies_a_private_destination_before_invoking_the_connection_provider() {
    let connections = Arc::new(RecordingConnections::default());
    let policy = Arc::new(
        HostEgressPolicy::new(
            Box::new(NoDns),
            isoplane_host::egress::ConnectionLimits::uniform(1).expect("connection limits"),
        )
        .with_connections(connections.clone()),
    );
    let fetcher = PinnedHttpFetcher::new(1024);
    let result = fetcher
        .fetch(
            attempt(&policy).await,
            target(SocketAddr::from(([127, 0, 0, 1], 1))),
            request(),
        )
        .await;
    assert!(
        matches!(result, Err(isoplane_abi::result::FutureError::Denied(_))),
        "the factory boundary must reject forbidden addresses before opening sockets"
    );
    assert!(
        connections.calls.lock().is_empty(),
        "a destination policy denial must never reach socket creation"
    );
}

async fn streamed_response(
    frames: Vec<hyper::body::Frame<bytes::Bytes>>,
) -> (Box<dyn ResponseBody>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the response server must bind");
    let address = listener
        .local_addr()
        .expect("the response server must have an address");
    let frames = parking_lot::Mutex::new(Some(frames));
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("the fetch must connect");
        hyper::server::conn::http1::Builder::new()
            .serve_connection(
                hyper_util::rt::TokioIo::new(socket),
                hyper::service::service_fn(move |_| {
                    let body = http_body_util::StreamBody::new(futures::stream::iter(
                        frames
                            .lock()
                            .take()
                            .expect("the fixture serves one request")
                            .into_iter()
                            .map(Ok::<_, std::convert::Infallible>),
                    ));
                    async move {
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .header("connection", "close")
                                .header("trailer", "x-check")
                                .body(body)
                                .expect("the test response must be valid"),
                        )
                    }
                }),
            )
            .await
            .expect("the response server must complete");
    });
    let policy = Arc::new(
        HostEgressPolicy::new(
            Box::new(NoDns),
            isoplane_host::egress::ConnectionLimits::uniform(1).expect("connection limits"),
        )
        .with_destinations(Arc::new(ExactPeer(address))),
    );
    let fetcher = PinnedHttpFetcher::new(0);
    let mut req = request();
    req.headers.push(("te".into(), "trailers".into()));
    let response = fetcher
        .fetch(attempt(&policy).await, target(address), req)
        .await
        .expect("the response must start");
    let OutboundBody::Stream(body) = response.body else {
        panic!("an unknown-length response must be streamed")
    };
    (body, server)
}

#[tokio::test]
async fn small_http_body_reads_preserve_every_byte_of_a_larger_network_frame() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let expected: Vec<u8> = (0..8192).map(|index| (index % 251) as u8).collect();
        let (mut body, server) = streamed_response(vec![hyper::body::Frame::data(
            bytes::Bytes::from(expected.clone()),
        )])
        .await;
        let mut actual = Vec::new();
        loop {
            let chunk = body.read(3).await.expect("the stream must remain readable");
            assert!(
                chunk.data.len() <= 3,
                "each read must respect the requested buffer size"
            );
            actual.extend_from_slice(&chunk.data);
            if chunk.eof {
                break;
            }
        }
        assert_eq!(
            actual, expected,
            "a small guest read must retain the unread remainder for later reads"
        );
        server.await.expect("the response server must stop cleanly");
    })
    .await
    .expect("body reads must reach EOF before the hung-test deadline");
}

#[tokio::test]
async fn http_trailers_do_not_appear_as_an_empty_unfinished_body_chunk() {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut trailers = hyper::HeaderMap::new();
        trailers.insert("x-check", hyper::header::HeaderValue::from_static("done"));
        let (mut body, server) = streamed_response(vec![
            hyper::body::Frame::data(bytes::Bytes::from_static(b"A")),
            hyper::body::Frame::trailers(trailers),
        ])
        .await;
        let first = body.read(8).await.expect("the payload must arrive");
        assert_eq!(
            first.data, b"A",
            "the body reader must deliver the response data"
        );
        let last = body.read(8).await.expect("trailers must terminate cleanly");
        assert!(
            last.eof && last.data.is_empty(),
            "trailing HTTP metadata must not masquerade as more body data"
        );
        server.await.expect("the response server must stop cleanly");
    })
    .await
    .expect("trailers must settle before the hung-test deadline");
}
