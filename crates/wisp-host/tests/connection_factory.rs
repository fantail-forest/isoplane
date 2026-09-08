//! HTTP uses the same injected socket origin as ordinary managed TCP.
#[path = "common/connection_factory.rs"]
mod connection_factory;

use connection_factory::RecordingConnections;
use std::net::SocketAddr;
use std::sync::Arc;
use wisp_host::bindings::Fetcher;
use wisp_host::egress::{
    ConnectionOwner, DenyReason, DestinationPolicy, DnsError, HostEgressPolicy, Resolver,
};
use wisp_host::{InstanceId, PinnedHttpFetcher};

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
fn request() -> wisp_abi::calls::FetchRequest {
    wisp_abi::calls::FetchRequest {
        method: "GET".into(),
        url: "http://owned.test/".into(),
        headers: vec![],
        body: vec![],
    }
}
fn target(address: SocketAddr) -> wisp_abi::host::PinnedTarget {
    wisp_abi::host::PinnedTarget {
        url: "http://owned.test/".into(),
        addr: address,
    }
}
fn owner() -> ConnectionOwner {
    ConnectionOwner::new("factory-tenant".into(), InstanceId(91))
        .expect("the test has an explicit tenant")
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
        HostEgressPolicy::new(Box::new(NoDns), 1, 1024)
            .with_destinations(Arc::new(ExactPeer(address)))
            .with_connections(connections.clone()),
    );
    let fetcher = PinnedHttpFetcher::new(policy, 1024);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        fetcher.fetch(owner(), target(address), request()),
    )
    .await
    .expect("the owned HTTP request must finish before its test deadline")
    .expect("HTTP must use the working provider");
    assert_eq!(
        response.body_inline.as_deref(),
        Some(b"provider".as_slice()),
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
    let policy =
        Arc::new(HostEgressPolicy::new(Box::new(NoDns), 1, 1024).with_connections(connections));
    let fetcher = PinnedHttpFetcher::new(policy, 1024);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        fetcher.fetch(owner(), target(expected), request()),
    )
    .await
    .expect("a substituted connection must be refused before the test deadline");
    assert!(
        matches!(result, Err(wisp_abi::result::FutureError::Io(_))),
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
        HostEgressPolicy::new(Box::new(NoDns), 1, 1024).with_connections(connections.clone()),
    );
    let fetcher = PinnedHttpFetcher::new(policy, 1024);
    let result = fetcher
        .fetch(
            owner(),
            target(SocketAddr::from(([127, 0, 0, 1], 1))),
            request(),
        )
        .await;
    assert!(
        matches!(result, Err(wisp_abi::result::FutureError::Denied(_))),
        "the factory boundary must reject forbidden addresses before opening sockets"
    );
    assert!(
        connections.calls.lock().is_empty(),
        "a destination policy denial must never reach socket creation"
    );
}
