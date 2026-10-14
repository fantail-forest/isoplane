//! Outbound HTTP. [`PinnedHttpFetcher`] connects to the address the egress
//! policy pinned and uses the URL only for the request line and the `Host`
//! header; nothing here resolves a name, which is what makes DNS rebinding
//! impossible after the check. This legacy capability supports plain HTTP only;
//! HTTPS belongs to the guest socket/TLS implementation.

use crate::egress::{ConnectAttempt, ConnectionPermit};
use std::net::SocketAddr;

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

/// Sends HTTP over the connection the instance's attempt opens.
pub struct PinnedHttpFetcher {
    inline_limit: usize,
}

impl PinnedHttpFetcher {
    /// Use the node's inline response threshold.
    pub fn new(inline_limit: usize) -> Self {
        Self { inline_limit }
    }

    async fn send(
        &self,
        attempt: ConnectAttempt,
        addr: SocketAddr,
        req: Request<Full<Bytes>>,
    ) -> Result<(Response<Incoming>, ConnectionTask), FutureError> {
        let stream = attempt.connect(addr).await.map_err(|error| match error {
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
        let task = tokio::spawn(crate::reactor_accounting::inherit(async move {
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "egress connection closed");
            }
        }));
        let abort = task.abort_handle();
        let response = sender
            .send_request(req)
            .await
            .map_err(|e| FutureError::Io(format!("request: {e}")))?;
        // In progress until the response headers arrive, which is now.
        Ok((
            response,
            ConnectionTask {
                abort,
                _permit: attempt.established(),
            },
        ))
    }
}

#[async_trait::async_trait]
impl Fetcher for PinnedHttpFetcher {
    async fn fetch(
        &self,
        attempt: ConnectAttempt,
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

        let (resp, connection) = self.send(attempt, target.addr, request).await?;
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

/// The connection's driver and the open permit it holds until the driver is stopped.
struct ConnectionTask {
    abort: tokio::task::AbortHandle,
    _permit: ConnectionPermit,
}

impl Drop for ConnectionTask {
    fn drop(&mut self) {
        self.abort.abort();
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
    use crate::egress::{ConnectionFactory, ConnectionOwner, DnsError, HostEgressPolicy, Resolver};
    use std::future::Future;
    use std::sync::Arc;

    fn owner() -> ConnectionOwner {
        ConnectionOwner::new("tenant".into(), crate::InstanceId(1)).expect("identified owner")
    }

    async fn attempt(egress: &Arc<HostEgressPolicy>) -> ConnectAttempt {
        egress
            .begin_connect(owner())
            .await
            .expect("the instance may start a connection")
    }

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
        let egress = Arc::new(
            HostEgressPolicy::new(
                Box::new(NoNetwork),
                crate::egress::ConnectionLimits::uniform(8).expect("connection limits"),
            )
            .with_connections(Arc::new(NoNetwork)),
        );
        let fetcher = PinnedHttpFetcher::new(INLINE_BODY_LIMIT);
        let url = "https://public.example/".to_string();
        let result = fetcher
            .fetch(
                attempt(&egress).await,
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
        let egress = Arc::new(
            HostEgressPolicy::new(
                Box::new(NoNetwork),
                crate::egress::ConnectionLimits::uniform(8).expect("connection limits"),
            )
            .with_destinations(Arc::new(OnlyPeer(address))),
        );
        let fetcher = PinnedHttpFetcher::new(INLINE_BODY_LIMIT);
        let url = format!("http://pinned.example:{}/echo?x=1", address.port());
        let response = fetcher
            .fetch(
                attempt(&egress).await,
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
        assert_eq!(
            egress.instance_connections(crate::InstanceId(1)),
            0,
            "an inline response releases its connection before the fetch returns"
        );
        peer.await.expect("the peer task completes cleanly");
    }

    /// A response body that never ends, so its connection stays open.
    struct Endless;

    impl hyper::body::Body for Endless {
        type Data = Bytes;
        type Error = std::convert::Infallible;

        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            std::task::Poll::Pending
        }
    }

    #[tokio::test]
    async fn a_fetch_is_in_progress_until_its_response_headers_arrive() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local peer");
        let address = listener.local_addr().expect("the peer endpoint");
        let (received, request_arrived) = tokio::sync::oneshot::channel();
        let (respond, respond_gate) = tokio::sync::oneshot::channel::<()>();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("the pinned connection");
            let signals = std::sync::Mutex::new(Some((received, respond_gate)));
            let service = hyper::service::service_fn(move |_: Request<Incoming>| {
                let signals = signals.lock().expect("signals").take();
                async move {
                    let (received, respond_gate) = signals.expect("one request");
                    received.send(()).expect("the test awaits the request");
                    respond_gate.await.expect("the test releases the headers");
                    Ok::<_, std::convert::Infallible>(Response::new(Endless))
                }
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
        });
        let egress = Arc::new(
            HostEgressPolicy::new(
                Box::new(NoNetwork),
                crate::egress::ConnectionLimits::new(crate::egress::ConnectionCeilings {
                    in_progress: 1,
                    open: 8,
                })
                .expect("connection limits"),
            )
            .with_destinations(Arc::new(OnlyPeer(address))),
        );
        let fetcher = PinnedHttpFetcher::new(INLINE_BODY_LIMIT);
        let url = format!("http://pinned.example:{}/", address.port());
        let first = attempt(&egress).await;
        let fetch = tokio::spawn(async move {
            fetcher
                .fetch(
                    first,
                    PinnedTarget {
                        url: url.clone(),
                        addr: address,
                    },
                    FetchRequest {
                        method: "GET".into(),
                        url,
                        headers: vec![],
                        body: vec![],
                    },
                )
                .await
        });
        request_arrived.await.expect("the request reaches the peer");
        let mut waiting = Box::pin(egress.begin_connect(owner()));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            waiting.as_mut().poll(&mut context).is_pending(),
            "a fetch awaiting its response headers holds the instance's only slot"
        );
        respond.send(()).expect("the peer awaits the release");
        let response = fetch
            .await
            .expect("the fetch task")
            .expect("the fetch returns once headers arrive");
        assert!(
            matches!(response.body, OutboundBody::Stream(_)),
            "the response body is still open"
        );
        let std::task::Poll::Ready(Ok(next)) = waiting.as_mut().poll(&mut context) else {
            panic!("the attempt ends at the headers, so the next one starts");
        };
        assert_eq!(
            egress.instance_connections(crate::InstanceId(1)),
            2,
            "the open response body keeps its connection's permit beside the new attempt's"
        );
        drop(response);
        assert_eq!(
            egress.instance_connections(crate::InstanceId(1)),
            1,
            "dropping the body releases its connection's permit"
        );
        drop(next);
        peer.abort();
    }
}
