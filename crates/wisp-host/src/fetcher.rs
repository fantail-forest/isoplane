//! Outbound HTTP. [`PinnedHttpFetcher`] connects to the address the egress
//! policy pinned and uses the URL only for the request line, the `Host`
//! header and the TLS server name; nothing here resolves a name, which is
//! what makes DNS rebinding impossible after the check. `https://` egress
//! terminates TLS here, verifying the server against the host's trust
//! anchors, so a guest never holds a key or a root.

use crate::egress::{ConnectionError, ConnectionOwner, HostEgressPolicy};
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use wisp_abi::calls::FetchRequest;
use wisp_abi::host::PinnedTarget;
use wisp_abi::result::{BodyChunk, FutureError};

use crate::bindings::{
    BodyFrame, ExchangeFailure, Fetcher, OutboundBody, OutboundRequest, OutboundResponse,
    RequestBody, ResponseBody, TlsFailure,
};

/// Bodies at or below this size return inline and release their transport
/// before the fetch completes.
pub const INLINE_BODY_LIMIT: usize = 256 * 1024;

/// A request body error: the guest abandoned the body it was streaming.
#[derive(Debug)]
struct BodyAbandoned;

impl std::fmt::Display for BodyAbandoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the request body was abandoned before it was finished")
    }
}

impl std::error::Error for BodyAbandoned {}

type OutgoingBody = BoxBody<Bytes, BodyAbandoned>;

/// Sends HTTP through the same connection provider as managed TCP.
pub struct PinnedHttpFetcher {
    egress: Arc<HostEgressPolicy>,
    inline_limit: usize,
    roots: rustls::RootCertStore,
}

impl PinnedHttpFetcher {
    /// Use the node's egress provider and inline response threshold, trusting
    /// the operating system's certificate store for `https://`.
    pub fn new(egress: Arc<HostEgressPolicy>, inline_limit: usize) -> Self {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for error in &native.errors {
            tracing::warn!(%error, "a system trust anchor could not be loaded");
        }
        let (added, ignored) = roots.add_parsable_certificates(native.certs);
        tracing::debug!(added, ignored, "system trust anchors loaded");
        Self {
            egress,
            inline_limit,
            roots,
        }
    }

    /// Also trust the certificates in `pem`, for destinations under a
    /// private authority.
    pub fn with_trust_anchor(mut self, pem: &str) -> Result<Self, String> {
        let certificates: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(pem.as_bytes())
                .collect::<Result<_, _>>()
                .map_err(|error| format!("trust anchor is not PEM: {error}"))?;
        if certificates.is_empty() {
            return Err("trust anchor PEM holds no certificate".into());
        }
        for certificate in certificates {
            self.roots
                .add(certificate)
                .map_err(|error| format!("trust anchor is not a CA certificate: {error}"))?;
        }
        Ok(self)
    }

    fn tls(&self) -> Result<tokio_rustls::TlsConnector, ExchangeFailure> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| ExchangeFailure::Internal(format!("TLS versions: {error}")))?
            .with_root_certificates(self.roots.clone())
            .with_no_client_auth();
        Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
    }

    async fn transport(
        &self,
        owner: &ConnectionOwner,
        addr: SocketAddr,
        host: &str,
        tls: bool,
    ) -> Result<Box<dyn Transport>, ExchangeFailure> {
        let stream = self
            .egress
            .connect(owner, addr)
            .await
            .map_err(ExchangeFailure::Connect)?;
        if !tls {
            return Ok(Box::new(stream));
        }
        let name = ServerName::try_from(host.to_owned()).map_err(|error| {
            ExchangeFailure::Uri(format!("host is not a TLS server name: {error}"))
        })?;
        let stream = self
            .tls()?
            .connect(name, stream)
            .await
            .map_err(|error| ExchangeFailure::Tls(classify_tls(&error)))?;
        Ok(Box::new(stream))
    }

    async fn send(
        &self,
        owner: &ConnectionOwner,
        addr: SocketAddr,
        host: &str,
        tls: bool,
        req: Request<OutgoingBody>,
    ) -> Result<(Response<Incoming>, ConnectionTask), ExchangeFailure> {
        let stream = self.transport(owner, addr, host, tls).await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| ExchangeFailure::Protocol(format!("handshake: {e}")))?;
        let task = tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "egress connection closed");
            }
        });
        let connection = ConnectionTask(task.abort_handle());
        let response = sender
            .send_request(req)
            .await
            .map_err(|e| ExchangeFailure::Protocol(format!("request: {e}")))?;
        Ok((response, connection))
    }
}

/// A connected byte stream, plain or under TLS.
trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

fn classify_tls(error: &std::io::Error) -> TlsFailure {
    match error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        Some(rustls::Error::InvalidCertificate(_)) => TlsFailure::Certificate,
        Some(rustls::Error::AlertReceived(_)) => TlsFailure::Alert,
        _ => TlsFailure::Protocol,
    }
}

fn request_body(body: RequestBody) -> OutgoingBody {
    match body {
        RequestBody::Empty => Full::new(Bytes::new())
            .map_err(|never| match never {})
            .boxed(),
        RequestBody::Stream(receiver) => {
            let frames = futures::stream::unfold(Some(receiver), |receiver| async move {
                let mut receiver = receiver?;
                match receiver.recv().await {
                    Some(BodyFrame::Data(data)) => Some((Ok(Frame::data(data)), Some(receiver))),
                    Some(BodyFrame::End) => None,
                    // The sender went away without finishing: the request
                    // must not look complete to the peer.
                    None => Some((Err(BodyAbandoned), None)),
                }
            });
            BodyExt::boxed(StreamBody::new(frames))
        }
    }
}

#[async_trait::async_trait]
impl Fetcher for PinnedHttpFetcher {
    async fn fetch(
        &self,
        owner: ConnectionOwner,
        target: PinnedTarget,
        req: FetchRequest,
    ) -> Result<OutboundResponse, FutureError> {
        let request = OutboundRequest {
            method: req.method,
            url: target.url.clone(),
            headers: req
                .headers
                .into_iter()
                .map(|(name, value)| (name, value.into_bytes()))
                .collect(),
            body: if req.body.is_empty() {
                RequestBody::Empty
            } else {
                let (sender, receiver) = mpsc::channel(2);
                // Both frames fit the channel; nothing waits on the guest.
                sender
                    .try_send(BodyFrame::Data(Bytes::from(req.body)))
                    .map_err(|error| FutureError::Io(format!("request body: {error}")))?;
                sender
                    .try_send(BodyFrame::End)
                    .map_err(|error| FutureError::Io(format!("request body: {error}")))?;
                RequestBody::Stream(receiver)
            },
        };
        self.exchange(owner, target, request)
            .await
            .map_err(|failure| match failure {
                ExchangeFailure::Uri(reason) => FutureError::Denied(reason),
                ExchangeFailure::Connect(ConnectionError::Denied(reason)) => {
                    FutureError::Denied(reason.to_string())
                }
                other => FutureError::Io(other.to_string()),
            })
    }

    async fn exchange(
        &self,
        owner: ConnectionOwner,
        target: PinnedTarget,
        request: OutboundRequest,
    ) -> Result<OutboundResponse, ExchangeFailure> {
        let parsed = crate::egress::parse_url(&target.url)
            .map_err(|e| ExchangeFailure::Uri(e.to_string()))?;
        let tls = parsed.scheme == "https";
        let default_port = if tls { 443 } else { 80 };
        let authority = if parsed.port == default_port {
            parsed.host.clone()
        } else {
            format!("{}:{}", parsed.host, parsed.port)
        };
        let mut builder = Request::builder()
            .method(request.method.as_str())
            .uri(&parsed.path_and_query)
            .header(hyper::header::HOST, &authority);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_slice());
        }
        let hyper_request = builder
            .body(request_body(request.body))
            .map_err(|e| ExchangeFailure::Uri(format!("bad request: {e}")))?;

        let (resp, connection) = self
            .send(&owner, target.addr, &parsed.host, tls, hyper_request)
            .await?;
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).to_string(),
                )
            })
            .collect();

        let hint = resp
            .headers()
            .get(hyper::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());
        let body = resp.into_body();

        if hint.is_some_and(|n| n <= self.inline_limit) {
            let collected = body
                .collect()
                .await
                .map_err(|e| ExchangeFailure::Protocol(format!("body: {e}")))?
                .to_bytes();
            return Ok(OutboundResponse {
                status,
                headers,
                body: OutboundBody::Inline(collected.to_vec()),
            });
        }

        Ok(OutboundResponse {
            status,
            headers,
            body: OutboundBody::Stream(Box::new(IncomingBody {
                incoming: body,
                unread: Bytes::new(),
                _connection: connection,
            })),
        })
    }
}

struct ConnectionTask(tokio::task::AbortHandle);

impl Drop for ConnectionTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct IncomingBody {
    _connection: ConnectionTask,
    incoming: Incoming,
    unread: Bytes,
}

#[async_trait::async_trait]
impl ResponseBody for IncomingBody {
    async fn read(&mut self, max_len: usize) -> Result<BodyChunk, FutureError> {
        while self.unread.is_empty() {
            let Some(frame) = self.incoming.frame().await else {
                return Ok(BodyChunk {
                    data: Vec::new(),
                    eof: true,
                });
            };
            let frame = frame.map_err(|error| FutureError::Io(format!("body: {error}")))?;
            if let Ok(data) = frame.into_data() {
                self.unread = data;
            }
        }
        let length = if max_len == 0 {
            self.unread.len()
        } else {
            max_len.min(self.unread.len())
        };
        Ok(BodyChunk {
            data: self.unread.split_to(length).to_vec(),
            eof: false,
        })
    }
}
