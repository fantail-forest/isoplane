//! Outbound HTTP. [`PinnedHttpFetcher`] connects to the address the egress
//! policy pinned and uses the URL only for the request line and the `Host`
//! header; nothing here resolves a name, which is what makes DNS rebinding
//! impossible after the check. There is no TLS crate in the workspace, so
//! `https://` egress is refused rather than downgraded.

use crate::egress::{ConnectionOwner, HostEgressPolicy};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;

use wisp_abi::calls::FetchRequest;
use wisp_abi::host::PinnedTarget;
use wisp_abi::result::{BodyChunk, FetchResponse, FutureError};

use crate::bindings::Fetcher;

/// Bodies at or below this size come back inline, which makes the whole
/// `http_fetch` future Bounded and therefore lossless across hibernation.
pub const INLINE_BODY_LIMIT: usize = 256 * 1024;

/// Sends HTTP through the same connection provider as managed TCP.
pub struct PinnedHttpFetcher {
    egress: Arc<HostEgressPolicy>,
    bodies: Mutex<HashMap<u32, ResponseBody>>,
    next_handle: AtomicU32,
    inline_limit: usize,
}

impl PinnedHttpFetcher {
    /// Use the node's egress provider and inline response threshold.
    pub fn new(egress: Arc<HostEgressPolicy>, inline_limit: usize) -> Self {
        Self {
            egress,
            bodies: Mutex::new(HashMap::new()),
            next_handle: AtomicU32::new(1),
            inline_limit,
        }
    }

    async fn send(
        &self,
        owner: &ConnectionOwner,
        addr: SocketAddr,
        req: Request<Full<Bytes>>,
    ) -> Result<Response<Incoming>, FutureError> {
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
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "egress connection closed");
            }
        });
        sender
            .send_request(req)
            .await
            .map_err(|e| FutureError::Io(format!("request: {e}")))
    }
}

#[async_trait::async_trait]
impl Fetcher for PinnedHttpFetcher {
    async fn fetch(
        &self,
        owner: ConnectionOwner,
        target: PinnedTarget,
        req: FetchRequest,
    ) -> Result<FetchResponse, FutureError> {
        let parsed = crate::egress::parse_url(&target.url)
            .map_err(|e| FutureError::Denied(e.to_string()))?;
        if parsed.scheme == "https" {
            return Err(FutureError::Io(
                "https egress requires TLS, which this host does not build".into(),
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

        let resp = self.send(&owner, target.addr, request).await?;
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
            return Ok(FetchResponse {
                status,
                headers,
                body_handle: 0,
                body_inline: Some(collected.to_vec()),
            });
        }

        // Unknown or large: a streaming handle, whose future is Unbounded and
        // will not survive a hibernation.
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.bodies.lock().insert(
            handle,
            ResponseBody {
                incoming: body,
                unread: Bytes::new(),
            },
        );
        Ok(FetchResponse {
            status,
            headers,
            body_handle: handle,
            body_inline: None,
        })
    }

    async fn read_body(&self, handle: u32, max_len: usize) -> Result<BodyChunk, FutureError> {
        // Taken out and put back rather than held across the await.
        let Some(mut body) = self.bodies.lock().remove(&handle) else {
            return Err(FutureError::NotFound);
        };
        let chunk = body.read(max_len).await?;
        if !chunk.eof {
            self.bodies.lock().insert(handle, body);
        }
        Ok(chunk)
    }
}

struct ResponseBody {
    incoming: Incoming,
    unread: Bytes,
}

impl ResponseBody {
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
