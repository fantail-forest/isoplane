//! A dispatch for tests that need the guest to run but not what it is asked.

/// A `DispatchRecord` with nothing interesting in it.
pub fn http_dispatch(id: u64) -> isoplane_abi::dispatch::DispatchRecord {
    use isoplane_abi::dispatch::*;
    DispatchRecord {
        dispatch_id: id,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: "https://example.test/x".into(),
            path: "/x".into(),
            query: vec![],
            headers: vec![("host".into(), "example.test".into())],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 0,
        occupancy_budget_ms: 30_000,
        now_ms: 1_700_000_000_000,
    }
}
