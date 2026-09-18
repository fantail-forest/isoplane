//! The result record: how a host future completes.

use serde::{Deserialize, Serialize};

/// Delivered to the guest via `isoplane_wake(future_id, result)`. The payload is
/// opaque here; the guest SDK recorded which typed response the future expects
/// when it issued the import call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultRecord {
    Ok(Vec<u8>),
    Err(FutureError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FutureError {
    /// The dispatch's cancellation future completed.
    Cancelled,
    /// Host capability policy refused the call, such as a denied egress target.
    Denied(String),
    NotFound,
    Io(String),
}


impl std::fmt::Display for FutureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FutureError::Cancelled => write!(f, "dispatch cancelled"),
            FutureError::Denied(why) => write!(f, "capability denied: {why}"),
            FutureError::NotFound => write!(f, "not found"),
            FutureError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for FutureError {}

impl FutureError {
    /// True for refusals that mean stop, as opposed to retry.
    pub fn is_terminal(&self) -> bool {
        matches!(self, FutureError::Cancelled)
    }
}

impl ResultRecord {
    pub fn ok<T: Serialize>(v: &T) -> Result<Self, postcard::Error> {
        Ok(ResultRecord::Ok(postcard::to_allocvec(v)?))
    }
    pub fn is_err(&self) -> bool {
        matches!(self, ResultRecord::Err(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Handle for streaming the body via `http_body_read`.
    pub body_handle: u32,
    /// The whole body, when it fit under the inline cap and no streaming read
    /// is needed.
    pub body_inline: Option<Vec<u8>>,
}



#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyChunk {
    pub data: Vec<u8>,
    pub eof: bool,
}



