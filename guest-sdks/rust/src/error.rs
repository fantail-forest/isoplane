//! Typed guest-side errors.

use core::fmt;

use wisp_abi::result::{FutureError, };

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WispError {
    /// A deadline elapsed, or the dispatch CPU budget did.
    Timeout,
    /// The dispatch's implicit cancellation future completed. Observed at an
    /// await, because cancellation is not an entrancy.
    Cancelled,
    /// Host capability policy refused the call, such as a denied egress target.
    Denied(String),
    NotFound,
    Io(String),
    /// The host sent something this SDK could not decode. Distinct from the
    /// above because it means the ABI, not the world, is broken.
    Protocol(String),
}

pub type Result<T> = core::result::Result<T, WispError>;

impl WispError {


}

impl From<FutureError> for WispError {
    fn from(e: FutureError) -> Self {
        match e {
            FutureError::Timeout => WispError::Timeout,
            FutureError::Cancelled => WispError::Cancelled,
            FutureError::Denied(s) => WispError::Denied(s),
            FutureError::NotFound => WispError::NotFound,
            FutureError::Io(s) => WispError::Io(s),
        }
    }
}

impl fmt::Display for WispError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WispError::Timeout => write!(f, "timeout"),
            WispError::Cancelled => write!(f, "cancelled"),
            WispError::Denied(s) => write!(f, "denied: {s}"),
            WispError::NotFound => write!(f, "not found"),
            WispError::Io(s) => write!(f, "io: {s}"),
            WispError::Protocol(s) => write!(f, "protocol: {s}"),
        }
    }
}

impl std::error::Error for WispError {}

