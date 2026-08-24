//! `result-record` — how a host future completes (spec.md §5.2, §6.8).

use serde::{Deserialize, Serialize};

/// Delivered to the guest via `wisp_wake(future_id, result)`.
///
/// The payload is opaque at this layer: the guest SDK knows which typed
/// response a given future expects because it recorded that when it issued
/// the import call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResultRecord {
    Ok(Vec<u8>),
    Err(FutureError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FutureError {
    Timeout,
    /// The dispatch's implicit cancellation future completed (§5.4).
    Cancelled,
    /// Host capability policy refused the call, such as a denied egress target.
    Denied(String),
    NotFound,
    Io(String),
}


impl ResultRecord {
    pub fn ok<T: Serialize>(v: &T) -> Result<Self, postcard::Error> {
        Ok(ResultRecord::Ok(postcard::to_allocvec(v)?))
    }
    pub fn is_err(&self) -> bool {
        matches!(self, ResultRecord::Err(_))
    }
}

// ---------------------------------------------------------------------------
// Typed payloads carried inside `ResultRecord::Ok`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// A response-body read completes through a host-owned future.
    pub body_handle: u32,
    /// Complete response bytes when returned inline instead of through `body_handle`.
    pub body_inline: Option<Vec<u8>>,
}



#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyChunk {
    pub data: Vec<u8>,
    pub eof: bool,
}



