//! `dispatch-record` — the single envelope covering every trigger type.

use serde::{Deserialize, Serialize};

/// One top-level unit of work handed to a guest via `wisp_handle`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchRecord {
    pub dispatch_id: u64,
    pub kind: DispatchKind,
    /// W3C context, propagated into outbound fetches automatically.
    pub trace: TraceContext,
    /// Cancellation is NOT an entrancy. The host cancels by completing
    /// this future; the guest observes it at its next await as a tripped
    /// `CancellationToken`. A dispatch that never awaits cannot observe it and
    /// is killed at its CPU budget instead — abruptly, with no unwinding.
    pub cancel_future: u64,
    /// CPU-ms budget for this dispatch, from the plan limits.
    pub cpu_budget_ms: u32,
    /// Coarse wall clock at dispatch start; frozen for the slice.
    pub now_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DispatchKind {
    Http(HttpRequest),
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
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
    /// Inline response bytes; streamed responses send subsequent chunks through `resp_write`.
    pub body: Vec<u8>,
    /// If true the guest will keep writing chunks after returning `done`.
    pub streaming: bool,
}

impl DispatchKind {


    pub fn label(&self) -> &'static str {
        match self {
            DispatchKind::Http(_) => "http",
        }
    }
}
