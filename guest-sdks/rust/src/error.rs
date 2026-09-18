//! Typed guest-side errors.

use core::fmt;

use isoplane_abi::result::{FutureError, };

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsoplaneError {
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

pub type Result<T> = core::result::Result<T, IsoplaneError>;

impl IsoplaneError {


}

impl From<FutureError> for IsoplaneError {
    fn from(e: FutureError) -> Self {
        match e {
            FutureError::Cancelled => IsoplaneError::Cancelled,
            FutureError::Denied(s) => IsoplaneError::Denied(s),
            FutureError::NotFound => IsoplaneError::NotFound,
            FutureError::Io(s) => IsoplaneError::Io(s),
        }
    }
}

impl fmt::Display for IsoplaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IsoplaneError::Cancelled => write!(f, "cancelled"),
            IsoplaneError::Denied(s) => write!(f, "denied: {s}"),
            IsoplaneError::NotFound => write!(f, "not found"),
            IsoplaneError::Io(s) => write!(f, "io: {s}"),
            IsoplaneError::Protocol(s) => write!(f, "protocol: {s}"),
        }
    }
}

impl std::error::Error for IsoplaneError {}

