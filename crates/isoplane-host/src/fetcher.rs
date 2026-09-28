//! Outbound HTTP. [`PinnedHttpFetcher`] connects to the address the egress
//! policy pinned and uses the URL only for the request line and the `Host`
//! header; nothing here resolves a name, which is what makes DNS rebinding
//! impossible after the check. This legacy capability supports plain HTTP only;
//! HTTPS belongs to the guest socket/TLS implementation.

use crate::egress::{ConnectionOwner, HostEgressPolicy};
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;

use isoplane_abi::calls::FetchRequest;
use isoplane_abi::host::PinnedTarget;
use isoplane_abi::result::{BodyChunk, FutureError};

use crate::bindings::{Fetcher, OutboundBody, OutboundResponse, ResponseBody};

/// Bodies at or below this size return inline and release their transport
/// before the fetch completes.
pub const INLINE_BODY_LIMIT: usize = 256 * 1024;

/// Sends HTTP through the same connection provider as managed TCP.
pub struct PinnedHttpFetcher {
    egress: Arc<HostEgressPolicy>,
    inline_limit: usize,
}

impl PinnedHttpFetcher {
    /// Use the node's egress provider and inline response threshold.
    pub fn new(egress: Arc<HostEgressPolicy>, inline_limit: usize) -> Self {
        Self {
            egress,
            inline_limit,
        }
    }

    async fn send(
        &self,
        owner: &ConnectionOwner,
        addr: SocketAddr,
        req: Request<Full<Bytes>>,
    ) -> Result<(Response<Incoming>, ConnectionTask), FutureError> {
        let stream = self
            .egress
            .connect(owner, addr)
            .await
            .map_err(|error| match error {
                crate::egress::ConnectionError::Denied(reason) => {
                    FutureError::Denied(reason.to_string())
                }
                failure @ (crate::egress::ConnectionError::Io(_)
                | crate::egress::ConnectionError::PeerMismatch { .. }) => {
                    FutureError::Io(format!("connect {addr}: {failure}"))
                }
            })?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| FutureError::Io(format!("handshake: {e}")))?;
        let task = tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "egress connection closed");
            }
        });
        let connection = ConnectionTask(task.abort_handle());
        let response = sender
            .send_request(req)
            .await
            .map_err(|e| FutureError::Io(format!("request: {e}")))?;
        Ok((response, connection))
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
        let parsed = crate::egress::parse_url(&target.url)
            .map_err(|e| FutureError::Denied(e.to_string()))?;
        if parsed.scheme == "https" {
            return Err(FutureError::Io(
                "legacy http_fetch does not support HTTPS; use guest TLS over sockets".into(),
            ));
        }
        let authority = if parsed.port == 80 {
            parsed.host.clone()
        } else {
            format!("{}:{}", parsed.host, parsed.port)
        };
        let mut builder = Request::builder()
            .method(req.method.as_str())
            .uri(&parsed.path_and_query)
            .header(hyper::header::HOST, &authority);
        for (k, v) in &req.headers {
            builder = builder.header(k.as_str(), v.as_str());
        }
        let request = builder
            .body(Full::new(Bytes::from(req.body)))
            .map_err(|e| FutureError::Denied(format!("bad request: {e}")))?;

        let (resp, connection) = self.send(&owner, target.addr, request).await?;
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
                .map_err(|e| FutureError::Io(format!("body: {e}")))?
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::{ConnectionFactory, DnsError, Resolver};

    struct NoNetwork;

    #[async_trait::async_trait]
    impl Resolver for NoNetwork {
        async fn resolve(&self, host: &str, _: u16) -> Result<Vec<SocketAddr>, DnsError> {
            panic!("the pinned fetch must not resolve {host}");
        }
    }

    #[async_trait::async_trait]
    impl ConnectionFactory for NoNetwork {
        async fn connect(
            &self,
            _: &ConnectionOwner,
            destination: SocketAddr,
        ) -> std::io::Result<tokio::net::TcpStream> {
            panic!("legacy HTTPS must be refused before connecting to {destination}");
        }
    }

    #[tokio::test]
    async fn legacy_https_fetch_is_refused_before_any_host_tls_or_connection() {
        let egress =
            HostEgressPolicy::new(Box::new(NoNetwork), 8).with_connections(Arc::new(NoNetwork));
        let fetcher = PinnedHttpFetcher::new(Arc::new(egress), INLINE_BODY_LIMIT);
        let url = "https://public.example/".to_string();
        let result = fetcher
            .fetch(
                ConnectionOwner::new("tenant".into(), crate::InstanceId(1))
                    .expect("identified owner"),
                PinnedTarget {
                    url: url.clone(),
                    addr: "8.8.8.8:443".parse().expect("public endpoint"),
                },
                FetchRequest {
                    method: "GET".into(),
                    url,
                    headers: vec![],
                    body: vec![],
                },
            )
            .await;
        assert!(
            matches!(result, Err(FutureError::Io(_))),
            "legacy fetch must refuse HTTPS; TLS belongs to the guest socket path"
        );
    }
    struct OnlyPeer(SocketAddr);

    impl crate::egress::DestinationPolicy for OnlyPeer {
        fn authorize(&self, address: SocketAddr) -> Result<(), crate::egress::DenyReason> {
            if address == self.0 {
                Ok(())
            } else {
                Err(crate::egress::DenyReason::PrivateAddress(address.ip()))
            }
        }
    }

    #[tokio::test]
    async fn legacy_plain_http_still_sends_the_request_and_returns_the_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local peer");
        let address = listener.local_addr().expect("the peer endpoint");
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("the pinned connection");
            let service = hyper::service::service_fn(|request: Request<Incoming>| async move {
                assert_eq!(
                    request.method(),
                    "POST",
                    "the fetch method must reach the peer"
                );
                assert_eq!(
                    request.uri(),
                    "/echo?x=1",
                    "the path must come from the request URL"
                );
                let bytes = request
                    .into_body()
                    .collect()
                    .await
                    .expect("the request body")
                    .to_bytes();
                assert_eq!(
                    bytes.as_ref(),
                    b"payload",
                    "the complete request body must reach the peer"
                );
                Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from_static(
                    b"ok",
                ))))
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                .expect("the peer HTTP exchange");
        });
        let egress = HostEgressPolicy::new(Box::new(NoNetwork), 8)
            .with_destinations(Arc::new(OnlyPeer(address)));
        let fetcher = PinnedHttpFetcher::new(Arc::new(egress), INLINE_BODY_LIMIT);
        let url = format!("http://pinned.example:{}/echo?x=1", address.port());
        let response = fetcher
            .fetch(
                ConnectionOwner::new("tenant".into(), crate::InstanceId(1))
                    .expect("identified owner"),
                PinnedTarget {
                    url: url.clone(),
                    addr: address,
                },
                FetchRequest {
                    method: "POST".into(),
                    url,
                    headers: vec![],
                    body: b"payload".to_vec(),
                },
            )
            .await
            .expect("plain HTTP remains supported");
        assert_eq!(response.status, 200, "the peer's status must be returned");
        assert!(
            matches!(response.body, OutboundBody::Inline(ref body) if body == b"ok"),
            "the small response must be returned intact"
        );
        peer.await.expect("the peer task completes cleanly");
    }
}
