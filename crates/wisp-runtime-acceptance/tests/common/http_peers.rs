//! Owned HTTP peers for the `HttpClient` family: a server with one route
//! per behaviour, speaking HTTP/1.1 alone or HTTP/1.1 and HTTP/2 (h2c with
//! prior knowledge in plaintext, `h2` through ALPN behind TLS with the
//! fixed certificate material), a peer that closes its idle connection
//! after answering, an HTTP/2 peer that sends GOAWAY before its first
//! answer, and a reactor that admits exactly those peers and trusts nothing
//! on the guest's behalf.

use std::future::Future;
use std::io::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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

/// A running peer: its address, how many connections it accepted, and the
/// ALPN protocol each TLS handshake settled on.
#[derive(Clone)]
pub struct HttpPeer {
    pub address: SocketAddr,
    accepted: Arc<AtomicUsize>,
    alpn: Arc<Mutex<Vec<Option<String>>>>,
}

impl HttpPeer {
    pub fn connections(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// The protocol negotiated by each TLS handshake, in acceptance order;
    /// `None` where the client offered no ALPN.
    pub fn negotiated_alpn(&self) -> Vec<Option<String>> {
        self.alpn
            .lock()
            .expect("the ALPN record is only written by the peer")
            .clone()
    }
}

/// The HTTP versions a peer speaks.
#[derive(Clone, Copy)]
enum Protocols {
    /// HTTP/1.1 only; behind TLS it offers `http/1.1` alone through ALPN.
    Http1,
    /// HTTP/1.1 and HTTP/2: h2c with prior knowledge in plaintext, `h2`
    /// preferred over `http/1.1` through ALPN behind TLS.
    Http1And2,
}

impl Protocols {
    fn builder(self) -> auto::Builder<TokioExecutor> {
        let builder = auto::Builder::new(TokioExecutor::new());
        match self {
            Protocols::Http1 => builder.http1_only(),
            Protocols::Http1And2 => builder,
        }
    }

    fn alpn(self) -> &'static [&'static str] {
        match self {
            Protocols::Http1 => &["http/1.1"],
            Protocols::Http1And2 => &["h2", "http/1.1"],
        }
    }
}

/// Starts a plaintext peer speaking HTTP/1.1 and h2c; the listener lives as
/// long as the runtime.
pub async fn http_peer() -> HttpPeer {
    serve(None, Protocols::Http1And2).await
}

/// Starts an HTTP/1.1-only peer behind TLS presenting `certificate`.
pub async fn https_peer(certificate: Certificate) -> HttpPeer {
    serve(
        Some(tls_acceptor(certificate, Protocols::Http1)),
        Protocols::Http1,
    )
    .await
}

/// Starts a peer behind TLS presenting `certificate` that negotiates `h2`
/// through ALPN and falls back to HTTP/1.1.
pub async fn h2_https_peer(certificate: Certificate) -> HttpPeer {
    serve(
        Some(tls_acceptor(certificate, Protocols::Http1And2)),
        Protocols::Http1And2,
    )
    .await
}

fn tls_acceptor(certificate: Certificate, protocols: Protocols) -> TlsAcceptor {
    let (certificates, key) = certificate.presented();
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .expect("the test peer's TLS configuration is valid");
    config.alpn_protocols = protocols
        .alpn()
        .iter()
        .map(|p| p.as_bytes().to_vec())
        .collect();
    TlsAcceptor::from(Arc::new(config))
}

/// Starts a peer that answers one request per connection with a keep-alive
/// response and then closes the connection: the pooled connection a client
/// keeps is stale by the time it is reused.
pub async fn stale_peer() -> HttpPeer {
    let (listener, peer) = bind().await;
    accept_loop(listener, peer.accepted.clone(), |mut stream| async move {
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
    peer
}

/// Starts a plaintext HTTP/2 peer that sends GOAWAY on every connection
/// before answering its first request, and answers only once the client
/// has acknowledged a ping sent behind the GOAWAY, so the client has read
/// the GOAWAY before it reads the response. Only `/hello` is served.
pub async fn goaway_peer() -> HttpPeer {
    let (listener, peer) = bind().await;
    accept_loop(listener, peer.accepted.clone(), |stream| async move {
        let Ok(mut connection) = h2::server::handshake(stream).await else {
            return;
        };
        let mut pings = connection
            .ping_pong()
            .expect("the ping handle is taken once per connection");
        // `accept` drives the connection, so it is polled alongside the
        // ping; the loop runs until the connection closes.
        while let Some(Ok((_request, mut respond))) = connection.accept().await {
            connection.graceful_shutdown();
            tokio::select! {
                pong = pings.ping(h2::Ping::opaque()) => {
                    if pong.is_err() {
                        return;
                    }
                }
                _ = connection.accept() => return,
            }
            let response = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/plain")
                .body(())
                .expect("a static response builds");
            let Ok(mut body) = respond.send_response(response, false) else {
                return;
            };
            let _sent = body.send_data(Bytes::from_static(b"hello from peer"), true);
        }
    });
    peer
}

async fn bind() -> (TcpListener, HttpPeer) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("the test peer must bind");
    let address = listener.local_addr().expect("the test peer has an address");
    let peer = HttpPeer {
        address,
        accepted: Arc::new(AtomicUsize::new(0)),
        alpn: Arc::new(Mutex::new(Vec::new())),
    };
    (listener, peer)
}

/// Accepts connections for as long as the runtime lives, counting each and
/// serving it on its own task.
fn accept_loop<F, Fut>(listener: TcpListener, accepted: Arc<AtomicUsize>, connection: F)
where
    F: Fn(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            accepted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(connection(stream));
        }
    });
}

async fn serve(tls: Option<TlsAcceptor>, protocols: Protocols) -> HttpPeer {
    let (listener, peer) = bind().await;
    let alpn = peer.alpn.clone();
    accept_loop(listener, peer.accepted.clone(), move |stream| {
        let tls = tls.clone();
        let alpn = alpn.clone();
        async move {
            let service = service_fn(route);
            let served = match tls {
                Some(acceptor) => match acceptor.accept(stream).await {
                    Ok(stream) => {
                        let negotiated = stream
                            .get_ref()
                            .1
                            .alpn_protocol()
                            .map(|protocol| String::from_utf8_lossy(protocol).into_owned());
                        alpn.lock()
                            .expect("the ALPN record is only written by the peer")
                            .push(negotiated);
                        protocols
                            .builder()
                            .serve_connection(TokioIo::new(stream), service)
                            .await
                    }
                    // A handshake the client refuses ends the connection; that is the point of the stranger.
                    Err(_) => return,
                },
                None => {
                    protocols
                        .builder()
                        .serve_connection(TokioIo::new(stream), service)
                        .await
                }
            };
            if let Err(error) = served {
                // A client that cancels mid-response closes on the peer; nothing to assert here.
                let _ = error;
            }
        }
    });
    peer
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
