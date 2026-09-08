//! Typed requests for native readiness resources retained by the host.

pub mod canonical;

/// An instance-local resource name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceHandle(pub u32);

/// A duration in canonical WASI nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationNanos(pub u64);

/// A nonblocking operation on host-owned readiness resources.
#[derive(Debug)]
pub enum Request {
    /// Create a coarse monotonic deadline.
    Timer(DurationNanos),
    /// Inspect one pollable without waiting.
    Ready(ResourceHandle),
    /// Return indexes of ready pollables; refuse if all would require waiting.
    Poll(Vec<ResourceHandle>),
    /// Release a pollable and cancel work it owns.
    Drop(ResourceHandle),
}

/// The complete result of a native readiness operation.
#[derive(Debug)]
pub enum Response {
    /// The resource was released.
    Done,
    /// A newly allocated pollable.
    Handle(ResourceHandle),
    /// Readiness of a pollable.
    Ready(bool),
    /// Ready indexes into the caller's poll list.
    ReadyIndexes(Vec<u32>),
}

/// A readiness refusal at the host resource boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// This execution context has no resource authority.
    #[error("native readiness denied: {0}")]
    Denied(String),
    /// The operation would require unsupported synchronous waiting.
    #[error("unsupported native readiness operation: {0}")]
    Unsupported(&'static str),
    /// The resource does not belong to this instance.
    #[error("pollable {0:?} is not owned by this instance")]
    InvalidHandle(ResourceHandle),
    /// The instance has exhausted its resource budget.
    #[error("native readiness resource limit reached")]
    ResourceLimit,
}
