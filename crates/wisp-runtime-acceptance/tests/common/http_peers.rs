//! Owned HTTP peers for the `HttpClient` family: an HTTP/1.1 server with one
//! route per behaviour, served plain or behind TLS with the fixed
//! certificate material, a peer that closes its idle connection after
//! answering, and a reactor that admits exactly those peers and trusts
//! nothing on the guest's behalf.

use std::io::Write as _;
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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsAcceptor;
use wisp_harness::Reactor;
use wisp_host::{InstanceId, Resolver};

use crate::admitted::admitting;
pub use crate::tls_chains::Certificate;

/// A reactor whose outbound connections may only reach `peers`, resolving
/// names through `resolver`. The host holds no trust anchor: what the guest
/// trusts is the application's own root.
pub fn reactor_for_http(
    owner: u64,
    peers: Vec<SocketAddr>,
    resolver: Box<dyn Resolver>,
) -> Reactor {
    Reactor::new(InstanceId(owner), admitting(peers, resolver))
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
    let (certificates, key) = certificate.presented();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .expect("the test peer's TLS configuration is valid");
    serve(Some(TlsAcceptor::from(Arc::new(config)))).await
}

/// Starts a peer that answers one request per connection with a keep-alive
/// response and then closes the connection: the pooled connection a client
/// keeps is stale by the time it is reused.
pub async fn stale_peer() -> HttpPeer {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("the test peer must bind");
    let address = listener.local_addr().expect("the test peer has an address");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                // The request head ends at the blank line; nothing more is
                // needed to answer it.
                let mut head = Vec::new();
                let mut byte = [0u8];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte).await {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                }
                let body = b"served once";
                let mut response = Vec::new();
                let _written = write!(
                    response,
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                );
                response.extend_from_slice(body);
                let _sent = stream.write_all(&response).await;
                let _flushed = stream.flush().await;
                let _closed = stream.shutdown().await;
            });
        }
    });
    HttpPeer { address, accepted }
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

fn redirect(status: StatusCode, location: &str) -> Response<PeerBody> {
    Response::builder()
        .status(status)
        .header("location", location)
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from_static(b"moved")).boxed())
        .expect("a static response builds")
}

fn encoded(encoding: &str, body: Vec<u8>) -> Response<PeerBody> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .header("content-encoding", encoding)
        .body(Full::new(Bytes::from(body)).boxed())
        .expect("a static response builds")
}

/// The pattern the guest and the peer both generate for `length` bytes.
pub fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(bytes)
        .expect("a memory encoder accepts every byte");
    encoder.finish().expect("a memory encoder finishes")
}

fn deflate(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(bytes)
        .expect("a memory encoder accepts every byte");
    encoder.finish().expect("a memory encoder finishes")
}

fn header(request: &Request<Incoming>, name: &str) -> String {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("missing")
        .to_owned()
}

async fn route(request: Request<Incoming>) -> Result<Response<PeerBody>, std::convert::Infallible> {
    let path = request.uri().path().to_owned();
    let numbered = |prefix: &str| {
        path.strip_prefix(prefix)
            .and_then(|count| count.parse::<usize>().ok())
    };
    Ok(match path.as_str() {
        "/hello" => text(StatusCode::OK, "hello from peer"),
        "/world" => text(StatusCode::OK, "world"),
        "/redirect" => redirect(StatusCode::FOUND, "http://elsewhere.test/moved"),
        "/relative" => redirect(StatusCode::FOUND, "/hello"),
        "/replay" => redirect(StatusCode::TEMPORARY_REDIRECT, "/echo"),
        "/accept-encoding" => text(StatusCode::OK, &header(&request, "accept-encoding")),
        "/headers" => {
            let ping = header(&request, "x-wisp");
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
        _ => {
            if let Some(hops) = numbered("/hops/") {
                if hops == 0 {
                    text(StatusCode::OK, "landed")
                } else {
                    redirect(StatusCode::FOUND, &format!("/hops/{}", hops - 1))
                }
            } else if let Some(length) = numbered("/gzip/") {
                encoded("gzip", gzip(&pattern(length)))
            } else if let Some(length) = numbered("/deflate/") {
                encoded("deflate", deflate(&pattern(length)))
            } else if let Some(length) = numbered("/stream/") {
                // Chunked, without a content length, so the body streams.
                let chunks: Vec<Result<Frame<Bytes>, std::convert::Infallible>> = pattern(length)
                    .chunks(16 * 1024)
                    .map(|chunk| Ok(Frame::data(Bytes::copy_from_slice(chunk))))
                    .collect();
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/octet-stream")
                    .body(StreamBody::new(futures::stream::iter(chunks)).boxed())
                    .expect("a static response builds")
            } else {
                text(StatusCode::NOT_FOUND, "no such route")
            }
        }
    })
}
