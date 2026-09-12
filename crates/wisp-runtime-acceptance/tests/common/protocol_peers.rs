//! Owned HTTP and WebSocket peers, admitting no outside service.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::tls_material::{LEAF_CERTIFICATE, LEAF_KEY, ROOT_CERTIFICATE};
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use wisp_proxy::ws::{accept_key, WsReader, WsWriter};
use wisp_proxy::{WsError, WsFrame};

/// An HTTP request target and the presence of credential headers.
#[derive(Clone, Debug)]
pub struct ObservedRequest {
    /// The request target received by this peer.
    pub uri: String,
    /// Whether an origin authorization header was present.
    pub authorization: bool,
    /// Whether a proxy authorization header was present.
    pub proxy_authorization: bool,
}

/// An owned peer and the requests and close handshake it observed.
#[derive(Clone)]
pub struct ProtocolPeer {
    /// The bound address of the owned listener.
    pub address: SocketAddr,
    requests: Arc<Mutex<Vec<ObservedRequest>>>,
    closed: Arc<AtomicBool>,
    tunnel: Option<Tunnel>,
}

#[derive(Clone, Copy)]
struct Tunnel {
    destination: SocketAddr,
    authenticate: bool,
}

impl ProtocolPeer {
    /// Returns the requests observed so far, in arrival order.
    pub fn requests(&self) -> Vec<ObservedRequest> {
        self.requests
            .lock()
            .expect("the peer request record must remain usable")
            .clone()
    }

    /// Reports whether the peer received a WebSocket close frame.
    pub fn observed_close(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// Starts an HTTP/WebSocket peer, using the fixture certificate when secure.
pub async fn protocol_peer(secure: bool) -> ProtocolPeer {
    serve_peer(secure, None).await
}

/// Starts a CONNECT proxy restricted to the given owned TLS destination.
pub async fn connect_proxy(destination: SocketAddr, authenticate: bool) -> ProtocolPeer {
    serve_peer(
        false,
        Some(Tunnel {
            destination,
            authenticate,
        }),
    )
    .await
}

async fn serve_peer(secure: bool, tunnel: Option<Tunnel>) -> ProtocolPeer {
    let tls = secure.then(|| {
        let chain = format!("{LEAF_CERTIFICATE}\n{ROOT_CERTIFICATE}");
        let certificates = CertificateDer::pem_slice_iter(chain.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .expect("the secure protocol certificate chain must parse");
        let key = PrivateKeyDer::from_pem_slice(LEAF_KEY.as_bytes())
            .expect("the secure protocol private key must parse");
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .expect("the protocol peer's fixed certificate must match its key");
        TlsAcceptor::from(Arc::new(config))
    });
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("the owned protocol listener must bind");
    let peer = ProtocolPeer {
        address: listener
            .local_addr()
            .expect("the owned protocol listener must have an address"),
        requests: Arc::new(Mutex::new(Vec::new())),
        closed: Arc::new(AtomicBool::new(false)),
        tunnel,
    };
    let serving = peer.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let serving = serving.clone();
            let tls = tls.clone();
            tokio::spawn(async move {
                if let Some(tls) = tls {
                    let stream = tls
                        .accept(stream)
                        .await
                        .expect("the secure WebSocket client must complete TLS");
                    serve_connection(stream, serving).await;
                } else {
                    serve_connection(stream, serving).await;
                }
            });
        }
    });
    peer
}

async fn serve_connection<T>(stream: T, peer: ProtocolPeer)
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let result = hyper::server::conn::http1::Builder::new()
        .serve_connection(
            TokioIo::new(stream),
            service_fn(move |request| answer(peer.clone(), request)),
        )
        .with_upgrades()
        .await;
    if let Err(error) = result {
        assert!(error.is_incomplete_message(), "the protocol peer may only lose an incomplete request when its client cancels: {error}");
    }
}

async fn answer(
    peer: ProtocolPeer,
    mut request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let authorization = request
        .headers()
        .get("authorization")
        .is_some_and(|value| value == "Basic dXNlcjpwYXNz");
    let proxy_authorization = request
        .headers()
        .get("proxy-authorization")
        .is_some_and(|value| value == "Basic dXNlcjpwYXNz");
    peer.requests
        .lock()
        .expect("the peer request record must remain usable")
        .push(ObservedRequest {
            uri: request.uri().to_string(),
            authorization: request.headers().contains_key("authorization"),
            proxy_authorization: request.headers().contains_key("proxy-authorization"),
        });
    if request.method() == Method::CONNECT {
        let tunnel = peer
            .tunnel
            .expect("only the configured proxy may receive CONNECT");
        assert_eq!(
            request
                .uri()
                .authority()
                .map(|authority| authority.as_str()),
            Some(format!("leaf.acceptance.test:{}", tunnel.destination.port()).as_str()),
            "CONNECT must name the intended TLS origin rather than the proxy"
        );
        if tunnel.authenticate && !proxy_authorization {
            return Ok(Response::builder()
                .status(StatusCode::PROXY_AUTHENTICATION_REQUIRED)
                .header("proxy-authenticate", "Basic realm=\"fixture\"")
                .body(Full::new(Bytes::new()))
                .expect("the CONNECT authentication challenge must be valid"));
        }
        let upgrade = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            let stream = upgrade
                .await
                .expect("the CONNECT response must establish its tunnel");
            let mut stream = TokioIo::new(stream);
            let mut origin = TcpStream::connect(tunnel.destination)
                .await
                .expect("the owned TLS origin must accept the proxy connection");
            if let Err(error) = tokio::io::copy_bidirectional(&mut stream, &mut origin).await {
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ),
                    "the tunnel may only end when one of its peers closes: {error}"
                );
            }
        });
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::new()))
            .expect("the successful CONNECT response must be valid"));
    }
    let path = request.uri().path();
    if path.starts_with("/ws-") && path != "/ws-bad" {
        let key = request
            .headers()
            .get("sec-websocket-key")
            .expect("ClientWebSocket must send its handshake key")
            .to_str()
            .expect("the handshake key must be ASCII");
        let accept = accept_key(key);
        let upgrade = hyper::upgrade::on(&mut request);
        tokio::spawn(async move {
            let upgraded = upgrade
                .await
                .expect("the protocol peer must complete the accepted upgrade");
            let (read, write) = tokio::io::split(TokioIo::new(upgraded));
            let mut reader = WsReader::new(read, 4096);
            let mut writer = WsWriter::new(write);
            loop {
                let frame = match reader.next_message().await {
                    Ok(frame) => frame,
                    Err(WsError::Eof) => return,
                    Err(WsError::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::UnexpectedEof
                                | std::io::ErrorKind::BrokenPipe
                        ) =>
                    {
                        return
                    }
                    Err(error) => panic!("the guest must send valid WebSocket frames: {error}"),
                };
                match frame {
                    frame @ (WsFrame::Text(_) | WsFrame::Binary(_)) => writer
                        .write(&frame)
                        .await
                        .expect("the peer must echo the guest's message"),
                    WsFrame::Ping(bytes) => writer
                        .write(&WsFrame::Pong(bytes))
                        .await
                        .expect("the peer must answer ping"),
                    WsFrame::Pong(_) => {}
                    frame @ WsFrame::Close { .. } => {
                        peer.closed.store(true, Ordering::SeqCst);
                        writer
                            .write(&frame)
                            .await
                            .expect("the peer must answer the guest's close");
                        return;
                    }
                }
            }
        });
        return Ok(Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-accept", accept)
            .body(Full::new(Bytes::new()))
            .expect("the upgrade response must be valid"));
    }
    let (status, challenge) = match path {
        "/auth" if !authorization => (StatusCode::UNAUTHORIZED, Some("www-authenticate")),
        "/proxy-auth" if !proxy_authorization => (
            StatusCode::PROXY_AUTHENTICATION_REQUIRED,
            Some("proxy-authenticate"),
        ),
        _ => (StatusCode::OK, None),
    };
    let mut response = Response::builder().status(status);
    if let Some(header) = challenge {
        response = response.header(header, "Basic realm=\"fixture\"");
    }
    let body = if path == "/proxy" {
        request.uri().to_string()
    } else {
        "authenticated".to_owned()
    };
    Ok(response
        .body(Full::new(Bytes::from(body)))
        .expect("the protocol response must be valid"))
}
