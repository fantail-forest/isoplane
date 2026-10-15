//! The dispatch record: one envelope for every trigger a guest receives.

use serde::{Deserialize, Serialize};

/// One unit of work handed to a guest via `isoplane_handle`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchRecord {
    pub dispatch_id: u64,
    pub kind: DispatchKind,
    /// W3C trace context.
    pub trace: TraceContext,
    /// The future the host completes to cancel this dispatch. A guest observes
    /// it at its next await; one that never awaits is killed at its occupancy budget.
    pub cancel_future: u64,
    pub occupancy_budget_ms: u32,
    /// Wall clock at dispatch start, frozen for the slice.
    pub now_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DispatchKind {
    Http(HttpRequest),
    /// Runs the timers that came due while the instance was idle, and nothing
    /// else; the host sends it only to an application that opted in.
    Timers,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
    pub sampled: bool,
}

/// The response head a guest hands back via `resp_new`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseHead {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    #[serde(with = "serde_bytes")]
    pub body: Vec<u8>,
    /// The guest produces chunks through `resp_write` until `resp_finish`.
    /// Returning `done` with an unfinished stream keeps that dispatch active.
    pub streaming: bool,
}

impl DispatchKind {
    pub fn label(&self) -> &'static str {
        match self {
            DispatchKind::Http(_) => "http",
            DispatchKind::Timers => "timers",
        }
    }
}
