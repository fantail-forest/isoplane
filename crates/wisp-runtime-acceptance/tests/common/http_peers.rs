//! Owned HTTP peers for the `wasi:http` family: an HTTP/1.1 server with one
//! route per behaviour, served plain or behind TLS with the fixed
//! certificate material, and a reactor that admits exactly those peers.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;
use wisp_harness::Reactor;
use wisp_host::egress::{DenyReason, DestinationPolicy, HostEgressPolicy};
use wisp_host::{InstanceId, Resolver};

use crate::tls_material::{
    LEAF_CERTIFICATE, LEAF_KEY, ROOT_CERTIFICATE, STRANGER_CERTIFICATE, STRANGER_KEY,
};

/// A destination policy admitting exactly the test's own peers.
struct OnlyPeers(Vec<SocketAddr>);

impl DestinationPolicy for OnlyPeers {
    fn authorize(&self, peer: SocketAddr) -> Result<(), DenyReason> {
        if self.0.contains(&peer) {
            Ok(())
        } else {
            Err(DenyReason::NotAllowlisted(peer.to_string()))
        }
    }
}

/// A reactor whose outbound HTTP may only reach `peers`, resolving names
/// through `resolver`, trusting the material's root for TLS.
pub fn reactor_for_http(
    owner: u64,
    peers: Vec<SocketAddr>,
    resolver: Box<dyn Resolver>,
) -> Reactor {
    Reactor::with_trusted_root(
        InstanceId(owner),
        HostEgressPolicy::new(resolver, 8, 1 << 20).with_destinations(Arc::new(OnlyPeers(peers))),
        ROOT_CERTIFICATE,
    )
}

/// Waits for the reactor to release every connection permit: an aborted
/// exchange is dropped on the scheduler's next turn, so a test that just
/// cancelled one yields to it, failing rather than hanging past the deadline.
pub async fn released(reactor: &Reactor) {
    tokio::time::timeout(wisp_harness::WAKE_DEADLINE, async {
        while reactor.runtime().services.egress.connections() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the reactor must release the cancelled exchange's connection");
}

/// The certificate a TLS peer presents.
#[derive(Clone, Copy)]
pub enum Certificate {
    /// The leaf for `leaf.acceptance.test`, chained to the trusted root.
    Leaf,
    /// A self-signed certificate nothing trusts.
    Stranger,
}

/// A running peer: its address and how many connections it accepted.
pub struct HttpPeer {
    pub address: SocketAddr,
    accepted: Arc<AtomicUsize>,
}

impl HttpPeer {
    pub fn connections(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

/// Starts a plaintext peer; the listener lives as long as the runtime.
pub async fn http_peer() -> HttpPeer {
    serve(None).await
}

/// Starts a peer behind TLS presenting `certificate`.
pub async fn https_peer(certificate: Certificate) -> HttpPeer {
    let (chain, key) = match certificate {
        Certificate::Leaf => (LEAF_CERTIFICATE, LEAF_KEY),
        Certificate::Stranger => (STRANGER_CERTIFICATE, STRANGER_KEY),
    };
    let certificates: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(chain.as_bytes())
            .collect::<Result<_, _>>()
            .expect("the test material's certificate parses");
    let key =
        PrivateKeyDer::from_pem_slice(key.as_bytes()).expect("the test material's key parses");
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .expect("the test peer's TLS configuration is valid");
    serve(Some(TlsAcceptor::from(Arc::new(config)))).await
}

async fn serve(tls: Option<TlsAcceptor>) -> HttpPeer {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("the test peer must bind");
    let address = listener.local_addr().expect("the test peer has an address");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let tls = tls.clone();
            tokio::spawn(async move {
                let service = service_fn(route);
                let served = match tls {
                    Some(acceptor) => match acceptor.accept(stream).await {
                        Ok(stream) => {
                            http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service)
                                .await
                        }
                        // A handshake the client refuses ends the connection; that is the point of the stranger.
                        Err(_) => return,
                    },
                    None => {
                        http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), service)
                            .await
                    }
                };
                if let Err(error) = served {
                    // A client that cancels mid-response closes on the peer; nothing to assert here.
                    let _ = error;
                }
            });
        }
    });
    HttpPeer { address, accepted }
}

type PeerBody = BoxBody<Bytes, std::convert::Infallible>;

fn text(status: StatusCode, body: &str) -> Response<PeerBody> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from(body.to_owned())).boxed())
        .expect("a static response builds")
}

/// The pattern the guest and the peer both generate for `length` bytes.
pub fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

async fn route(request: Request<Incoming>) -> Result<Response<PeerBody>, std::convert::Infallible> {
    let path = request.uri().path().to_owned();
    Ok(match path.as_str() {
        "/hello" => text(StatusCode::OK, "hello from peer"),
        "/world" => text(StatusCode::OK, "world"),
        "/redirect" => Response::builder()
            .status(StatusCode::FOUND)
            .header("location", "http://elsewhere.test/moved")
            .header("content-type", "text/plain")
            .body(Full::new(Bytes::from_static(b"moved")).boxed())
            .expect("a static response builds"),
        "/headers" => {
            let ping = request
                .headers()
                .get("x-wisp")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("missing")
                .to_owned();
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/plain")
                .header("x-answer", "pong")
                .body(Full::new(Bytes::from(ping)).boxed())
                .expect("a static response builds")
        }
        "/echo" => {
            let body = request
                .into_body()
                .collect()
                .await
                .map(|collected| collected.to_bytes())
                .unwrap_or_default();
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/octet-stream")
                .body(Full::new(body).boxed())
                .expect("a static response builds")
        }
        "/stall" => {
            // Never answers: the client's timeout or cancellation ends it.
            std::future::pending::<()>().await;
            unreachable!("a stalled route never answers")
        }
        _ => match path
            .strip_prefix("/stream/")
            .and_then(|count| count.parse::<usize>().ok())
        {
            Some(length) => {
                // Chunked, without a content length, so the host streams it.
                let chunks: Vec<Result<Frame<Bytes>, std::convert::Infallible>> = pattern(length)
                    .chunks(16 * 1024)
                    .map(|chunk| Ok(Frame::data(Bytes::copy_from_slice(chunk))))
                    .collect();
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/octet-stream")
                    .body(StreamBody::new(futures::stream::iter(chunks)).boxed())
                    .expect("a static response builds")
            }
            None => text(StatusCode::NOT_FOUND, "no such route"),
        },
    })
}
