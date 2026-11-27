//! The result record: how a host future completes.

use serde::{Deserialize, Serialize};

/// What `isoplane_wake(future_id, record)` delivers: a completed future's
/// result, and the dispatch whose slice the wake begins. The slice runs only
/// that dispatch, and the status it returns is that dispatch's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeRecord {
    pub dispatch: u64,
    pub result: ResultRecord,
}

/// How a host future completes. The payload is opaque here; the guest SDK
/// recorded which typed response the future expects when it issued the import
/// call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultRecord {
    Ok(#[serde(with = "serde_bytes")] Vec<u8>),
    Err(FutureError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FutureError {
    /// The dispatch's cancellation future completed.
    Cancelled,
    /// Capability policy refused the call, such as an egress target outside the allowlist.
    Denied(String),
    Io(String),
    /// The instance cannot retain this result within its buffered-result byte limit.
    BufferLimit,
    /// The outbound response body the guest named is not open. Last, so the
    /// other variants encode the same with and without the feature.
    #[cfg(feature = "rust-guests")]
    NotFound,
}

impl ResultRecord {
    pub fn ok<T: Serialize>(v: &T) -> Result<Self, postcard::Error> {
        Ok(ResultRecord::Ok(postcard::to_allocvec(v)?))
    }

    /// The heap bytes this record holds: its payload's, or its error message's.
    pub fn heap_bytes(&self) -> usize {
        match self {
            ResultRecord::Ok(bytes) => bytes.capacity(),
            ResultRecord::Err(FutureError::Denied(message) | FutureError::Io(message)) => {
                message.capacity()
            }
            ResultRecord::Err(FutureError::Cancelled | FutureError::BufferLimit) => 0,
            #[cfg(feature = "rust-guests")]
            ResultRecord::Err(FutureError::NotFound) => 0,
        }
    }
}

#[cfg(feature = "rust-guests")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Handle for streaming the body via `http_body_read`.
    pub body_handle: u32,
    /// The whole body, when it fit under the inline cap and no streaming read
    /// is needed.
    #[serde(with = "serde_bytes")]
    pub body_inline: Option<Vec<u8>>,
}

#[cfg(feature = "rust-guests")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyChunk {
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
    pub eof: bool,
}
