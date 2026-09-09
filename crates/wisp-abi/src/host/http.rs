//! Host-only HTTP body ownership; these types are not serialized into guests.

use async_trait::async_trait;

use super::{HttpReply, Result};

/// Default queued-response high-water mark in bytes, independent of total response size.
pub const DEFAULT_RESPONSE_QUEUE_BYTES: usize = 256 * 1024;

/// A response body owned by its requester. Dropping an unfinished streaming body
/// cancels its dispatch. A read error ends the body; `None` is successful EOF.
#[async_trait]
pub trait HttpBody: Send + 'static {
    /// Exact remaining bytes when the body is already complete; otherwise unknown.
    fn remaining_bytes(&self) -> Option<u64> {
        None
    }

    /// Read the next body chunk, or successful EOF; an error terminates the body.
    async fn read(&mut self) -> Result<Option<Vec<u8>>>;
}

/// Response metadata available before its body finishes.
pub struct HttpExchange {
    /// Final HTTP response status.
    pub status: u16,
    /// Repeated application headers, validated by the proxy before transmission.
    pub headers: Vec<(String, String)>,
    /// Owned response body and its cancellation lifetime.
    pub body: Box<dyn HttpBody>,
}

impl HttpExchange {
    /// Adapt an already completed buffered dispatch without changing its payload.
    pub fn buffered(reply: HttpReply) -> Self {
        Self {
            status: reply.status,
            headers: reply.headers,
            body: Box::new(BufferedBody(Some(reply.body))),
        }
    }
}

struct BufferedBody(Option<Vec<u8>>);

#[async_trait]
impl HttpBody for BufferedBody {
    fn remaining_bytes(&self) -> Option<u64> {
        Some(self.0.as_ref().map_or(0, |body| body.len() as u64))
    }

    async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.0.take().filter(|body| !body.is_empty()))
    }
}
