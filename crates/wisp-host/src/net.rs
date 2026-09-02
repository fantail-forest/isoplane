//! Outbound HTTP through pinned destinations.
//!
//! ## Why the fetcher takes a `PinnedTarget` and not a URL
//!
//! Egress resolution is DNS-rebinding-safe: resolve, then pin. The
//! resolution and the policy decision happen in [`crate::egress`]; this
//! module's job is to not throw that away. [`PinnedHttpFetcher`] connects to
//! `target.addr` — the `SocketAddr` the policy already validated — and uses
//! the URL only for the request line and the `Host` header. There is no code
//! path here that resolves a name, which is the property that makes rebinding
//! structurally impossible rather than merely unlikely.
//!
//! ## PoC gap: no TLS
//!
//! The workspace has no TLS crate, so `https://` egress is refused with a
//! typed error rather than silently downgraded to `http://` — a silent
//! downgrade would be a security bug wearing a convenience costume. Plain HTTP
//! egress is real, and every egress control is exercised by it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};

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

/// Responses at or below this size are returned inline rather than through a body handle.
pub const INLINE_BODY_LIMIT: usize = 256 * 1024;

pub struct PinnedHttpFetcher {
    bodies: Mutex<HashMap<u32, Incoming>>,
    next_handle: AtomicU32,
    inline_limit: usize,
}

impl Default for PinnedHttpFetcher {
    fn default() -> Self {
        Self::new(INLINE_BODY_LIMIT)
    }
}

impl PinnedHttpFetcher {
    pub fn new(inline_limit: usize) -> Self {
        Self {
            bodies: Mutex::new(HashMap::new()),
            next_handle: AtomicU32::new(1),
            inline_limit,
        }
    }

    async fn send(
        addr: SocketAddr,
        req: Request<Full<Bytes>>,
    ) -> Result<Response<Incoming>, FutureError> {
        // Connect to the PINNED address. The hostname is never resolved here.
        let stream = tokio::net::TcpStream::connect(addr)
            .await
            .map_err(|e| FutureError::Io(format!("connect {addr}: {e}")))?;
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
        target: PinnedTarget,
        req: FetchRequest,
    ) -> Result<FetchResponse, FutureError> {
        let parsed = crate::egress::parse_url(&target.url)
            .map_err(|e| FutureError::Denied(e.to_string()))?;
        if parsed.scheme == "https" {
            // Refused, not downgraded. See the module docs.
            return Err(FutureError::Io(
                "https egress requires TLS, which the PoC does not build (see wisp-host/src/net.rs)"
                    .into(),
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
            // The trace context is propagated by the caller; everything
            // else is the tenant's.
            builder = builder.header(k.as_str(), v.as_str());
        }
        let request = builder
            .body(Full::new(Bytes::from(req.body)))
            .map_err(|e| FutureError::Denied(format!("bad request: {e}")))?;

        let resp = Self::send(target.addr, request).await?;
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

        // Unknown or large: hand back a streaming handle. That future is
        // typed Unbounded, so the guest already knows it will not survive a
        // hibernation and must be prepared to re-issue.
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.bodies.lock().insert(handle, body);
        Ok(FetchResponse {
            status,
            headers,
            body_handle: handle,
            body_inline: None,
        })
    }

    async fn read_body(&self, handle: u32, max_len: usize) -> Result<BodyChunk, FutureError> {
        // Taken out and put back rather than held across the await: a lock
        // held across an await on a reactor thread is how a reactor stops
        // owning all I/O and starts owning a deadlock.
        let Some(mut body) = self.bodies.lock().remove(&handle) else {
            return Err(FutureError::NotFound);
        };
        let frame = body.frame().await;
        match frame {
            None => Ok(BodyChunk {
                data: Vec::new(),
                eof: true,
            }),
            Some(Err(e)) => Err(FutureError::Io(format!("body: {e}"))),
            Some(Ok(f)) => {
                let data = f.into_data().map(|b| b.to_vec()).unwrap_or_default();
                let data = if max_len > 0 && data.len() > max_len {
                    // Trailing bytes beyond the guest's buffer would have to be
                    // stashed; the PoC truncates and says so rather than
                    // silently keeping a shadow buffer the guest cannot see.
                    tracing::warn!(
                        handle,
                        dropped = data.len() - max_len,
                        "body chunk truncated"
                    );
                    data[..max_len].to_vec()
                } else {
                    data
                };
                self.bodies.lock().insert(handle, body);
                Ok(BodyChunk { data, eof: false })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The scrape endpoint
// ---------------------------------------------------------------------------
