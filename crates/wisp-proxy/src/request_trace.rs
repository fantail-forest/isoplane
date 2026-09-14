//! Independent local request traces joined across forwarding by an opaque request ID.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Buf;
use hyper::body::{Body, Frame, SizeHint};
use hyper::header::{HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};

const REQUEST_ID: &str = "x-wisp-internal-request-id";

/// Whether request IDs were assigned by the host's forwarding boundary.
#[derive(Clone, Copy, Debug, Default)]
pub enum RequestOrigin {
    /// Replace client-supplied correlation values.
    #[default]
    Public,
    /// Accept the host ID on the private application-worker listener.
    Forwarded,
}

/// Host-generated correlation identity, separate from either process's trace ID.
#[derive(Clone, Copy, Debug)]
struct RequestId(u128);

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl RequestId {
    fn take(headers: &mut HeaderMap, origin: RequestOrigin) -> Self {
        let supplied = headers.remove(REQUEST_ID);
        let accepted = match origin {
            RequestOrigin::Public => None,
            RequestOrigin::Forwarded => supplied
                .as_ref()
                .and_then(|value| value.to_str().ok())
                .filter(|value| value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|value| u128::from_str_radix(value, 16).ok()),
        };
        Self(accepted.unwrap_or_else(rand::random))
    }
}

/// Owns a local request span until its response body ends, fails, or is dropped.
pub struct RequestTrace {
    span: tracing::Span,
    finished: bool,
    head_request: bool,
}

impl RequestTrace {
    /// Replace untrusted IDs and carry the new host ID to the private worker.
    pub fn frontend<B>(request: &mut Request<B>) -> Self {
        let id = RequestId::take(request.headers_mut(), RequestOrigin::Public);
        request.headers_mut().insert(
            REQUEST_ID,
            HeaderValue::from_str(&id.to_string())
                .expect("a fixed-width hexadecimal request ID is a valid header"),
        );
        Self {
            span: tracing::info_span!(parent: None, "frontend_request", request_id = %id,
                status = tracing::field::Empty, outcome = tracing::field::Empty),
            finished: false,
            head_request: request.method() == Method::HEAD,
        }
    }

    pub(crate) fn worker<B>(request: &mut Request<B>, origin: RequestOrigin) -> Self {
        let id = RequestId::take(request.headers_mut(), origin);
        Self {
            span: tracing::info_span!(parent: None, "worker_request", request_id = %id,
                status = tracing::field::Empty, outcome = tracing::field::Empty),
            finished: false,
            head_request: request.method() == Method::HEAD,
        }
    }

    /// The current process's span; it is never propagated as a remote trace parent.
    pub fn span(&self) -> tracing::Span {
        self.span.clone()
    }

    /// Retain this request through response framing, including early body cancellation.
    pub fn response<B: Body + Unpin>(self, response: Response<B>) -> Response<TracedBody<B>> {
        self.span.record("status", response.status().as_u16());
        let bodyless = self.head_request
            || response.status().is_informational()
            || matches!(
                response.status(),
                StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
            );
        response.map(|inner| TracedBody {
            remaining_bytes: if bodyless {
                Some(0)
            } else {
                inner.size_hint().exact()
            },
            inner,
            trace: self,
        })
    }

    fn finish(&mut self, outcome: &'static str) {
        if !self.finished {
            self.finished = true;
            self.span.record("outcome", outcome);
            self.span
                .in_scope(|| tracing::info!(event = "http_request_complete", outcome));
            self.span = tracing::Span::none();
        }
    }
}

impl Drop for RequestTrace {
    fn drop(&mut self) {
        self.finish("cancelled");
    }
}

/// HTTP body ownership that records completion without observing body contents.
pub struct TracedBody<B: Body + Unpin> {
    inner: B,
    trace: RequestTrace,
    remaining_bytes: Option<u64>,
}

impl<B: Body + Unpin> Body for TracedBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let result = this
            .trace
            .span
            .in_scope(|| Pin::new(&mut this.inner).poll_frame(cx));
        match &result {
            Poll::Ready(Some(Err(_))) => this.trace.finish("body_error"),
            Poll::Ready(None) => this.trace.finish("complete"),
            Poll::Ready(Some(Ok(frame))) => {
                if let (Some(remaining), Some(data)) = (&mut this.remaining_bytes, frame.data_ref())
                {
                    *remaining = remaining.saturating_sub(data.remaining() as u64);
                }
                if this.remaining_bytes == Some(0) || this.inner.is_end_stream() {
                    this.trace.finish("complete");
                }
            }
            Poll::Pending => {}
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<B: Body + Unpin> Drop for TracedBody<B> {
    fn drop(&mut self) {
        if self.remaining_bytes == Some(0) || self.inner.is_end_stream() {
            self.trace.finish("complete");
        }
    }
}

#[cfg(test)]
mod tests;
