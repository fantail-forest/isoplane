//! End-to-end through the **real guest exports**: a `DispatchRecord` goes in
//! through `isoplane_handle`, the guest parks, and `isoplane_wake` finishes it.
//!
//! This is the same code path a wasm engine drives; only the import vtable is
//! a stub. What it pins down is that a dispatch waiting on host work
//! **returns** `pending` with an empty stack, and the response only appears
//! after a wake.

use isoplane_guest::isoplane_abi::dispatch::{
    DispatchKind, DispatchRecord, HttpRequest, TraceContext,
};
use isoplane_guest::isoplane_abi::result::{ResultRecord, WakeRecord};
use isoplane_guest::isoplane_abi::status::PollStatus;
use isoplane_guest::testing::{with_stub_host, RESP_HANDLE};
use isoplane_guest::{Clock, Ctx, Handler, Request, Response};

struct Api;

impl Handler for Api {
    fn new() -> Self {
        Api
    }
    async fn handle(&'static self, req: Request, _ctx: Ctx) -> Response {
        match Clock::delay(1).await {
            Ok(()) => Response::new(200, req.path.into_bytes()),
            Err(e) => Response::text(502, format!("{e}")),
        }
    }
}

isoplane_guest::export_handler!(Api);

/// A wake for the dispatch `record` starts.
fn wake(result: ResultRecord) -> WakeRecord {
    WakeRecord {
        dispatch: 7,
        result,
    }
}

fn record(path: &str) -> DispatchRecord {
    DispatchRecord {
        dispatch_id: 7,
        kind: DispatchKind::Http(HttpRequest {
            method: "GET".into(),
            url: format!("https://x{path}"),
            path: path.into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        }),
        trace: TraceContext::default(),
        cancel_future: 999,
        occupancy_budget_ms: 30_000,
        pending_futures_limit: 64,
        now_ms: 1_700_000_000_000,
    }
}

/// Copy `bytes` into a guest buffer the way a host does: ask `isoplane_alloc`,
/// write, then hand back `(ptr, len)`.
fn handoff(bytes: &[u8]) -> (usize, usize) {
    let p = isoplane_alloc(bytes.len(), 8);
    assert_ne!(p, 0, "isoplane_alloc refused");
    // SAFETY: isoplane_alloc returned a distinct buffer with room for all bytes.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len()) };
    (p, bytes.len())
}

#[test]
fn a_dispatch_that_waits_parks_and_a_wake_completes_it() {
    with_stub_host(|host| {
        let enc = postcard::to_allocvec(&record("/hello")).unwrap();
        let (ptr, len) = handoff(&enc);

        // 1. The dispatch. The handler awaits `clock_deadline_in`, so this MUST park.
        let st = PollStatus::decode(isoplane_handle(ptr, len));
        assert_eq!(st, PollStatus::Pending, "an awaited host future must park");
        assert_eq!(isoplane_guest::rt::pending_futures(), 1);
        assert!(
            host.called("clock_deadline_in"),
            "the import must have been issued before parking"
        );
        assert!(
            !host.called("resp_new"),
            "no response may exist while parked"
        );

        // The stub mints future ids from 1, so `clock_deadline_in` got id 1.
        let fut_id = 1u64;

        // 2. The wake. A new slice, a fresh wasm stack.
        let res = ResultRecord::Ok(Vec::new());
        let enc = postcard::to_allocvec(&wake(res)).unwrap();
        let (ptr, len) = handoff(&enc);
        let st = PollStatus::decode(isoplane_wake(fut_id, ptr, len));

        assert_eq!(
            st,
            PollStatus::Done {
                response: RESP_HANDLE as u32
            }
        );
        assert!(host.called("resp_new"));
        assert_eq!(
            isoplane_guest::rt::pending_futures(),
            0,
            "future table must not leak"
        );
    });
}

#[test]
fn dispatch_and_wake_each_release_their_handoff_buffer() {
    with_stub_host(|host| {
        assert_eq!(host.outstanding_handoff_buffers(), 0);
        let enc = postcard::to_allocvec(&record("/hello")).unwrap();
        let (ptr, len) = handoff(&enc);
        assert_eq!(host.outstanding_handoff_buffers(), 1);

        let renc = postcard::to_allocvec(&wake(ResultRecord::Ok(Vec::new()))).unwrap();
        let (rp, rl) = handoff(&renc);
        assert_eq!(host.outstanding_handoff_buffers(), 2);

        assert_eq!(
            PollStatus::decode(isoplane_handle(ptr, len)),
            PollStatus::Pending
        );
        assert_eq!(
            host.outstanding_handoff_buffers(),
            1,
            "handle must release its buffer and retain the outstanding wake buffer"
        );

        assert_eq!(
            PollStatus::decode(isoplane_wake(1, rp, rl)),
            PollStatus::Done {
                response: RESP_HANDLE as u32
            }
        );
        assert_eq!(
            host.outstanding_handoff_buffers(),
            0,
            "wake must release its buffer"
        );
    });
}
