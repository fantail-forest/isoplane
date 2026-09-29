//! Per-dispatch context.

use core::future::Future;

use isoplane_abi::dispatch::{DispatchRecord, TraceContext};

use crate::future::{decode_unit, HostFuture};
use crate::rt;

/// Handed to every entrypoint. Cheap to clone; it is a view of the current
/// dispatch, not a resource.
#[derive(Clone)]
pub struct Ctx {
    pub dispatch_id: u64,
    /// Coarse wall clock at dispatch start, frozen for the slice.
    pub now_ms: u64,
    pub cpu_budget_ms: u32,
    pub trace: TraceContext,
    cancel_future: u64,
}

impl Ctx {
    #[doc(hidden)]
    pub fn from_record(rec: &DispatchRecord) -> Ctx {
        Ctx {
            dispatch_id: rec.dispatch_id,
            now_ms: rec.now_ms,
            cpu_budget_ms: rec.cpu_budget_ms,
            trace: rec.trace.clone(),
            cancel_future: rec.cancel_future,
        }
    }


    /// Schedule concurrent work during the active dispatch.
    /// The response does not wait for this task; arrange to await its result
    /// before completing. Host futures are cancelled when the dispatch ends.
    pub fn spawn(&self, fut: impl Future<Output = ()> + 'static) {
        rt::spawn(fut);
    }

    /// Cancellation is **not** an entrancy: the host never re-enters the guest
    /// to announce it. It completes this future, and the guest observes it at
    /// its next await. A dispatch that neither awaits nor checks
    /// [`crate::budget`] never observes cancellation and runs to the hard
    /// deadline at B+G, where it is trapped mid-frame with no unwinding.
    ///
    /// Use with [`crate::future::select2`].
    pub fn cancelled(&self) -> HostFuture<()> {
        HostFuture::new(self.cancel_future, decode_unit)
    }

    /// Instrumentation for the host-side park tests.
    pub fn park_count(&self) -> u64 {
        rt::park_count()
    }
    pub fn poll_count(&self) -> u64 {
        rt::poll_count()
    }
}
