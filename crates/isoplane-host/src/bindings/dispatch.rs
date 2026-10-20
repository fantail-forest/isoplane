//! What the host keeps for one dispatch on an instance. An instance can have
//! several dispatches in progress; each slice runs exactly one of them, and the
//! imports it calls act on that dispatch's state.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use super::cancellation::Cancellation;
use super::request_body::RequestBodyScope;
use crate::response_transport::ResponseTransport;

pub(crate) struct DispatchState {
    id: u64,
    pub(super) subrequests: AtomicU32,
    /// Bounds log volume independently of its occupied-worker cost.
    pub(super) log_bytes: AtomicU32,
    cancellation: Cancellation,
    /// Set once the dispatch's response is complete and it runs only the work
    /// it left behind.
    responded: AtomicBool,
    /// The dispatch's implicit cancellation future; the host cancels by
    /// completing it, never by entering the instance.
    cancel_future: AtomicU64,
    pub(crate) request_body: Mutex<Option<Arc<RequestBodyScope>>>,
    pub(crate) reactor_attribution: crate::reactor_accounting::Attribution,
    /// Where a streaming response for this dispatch goes, when its caller
    /// streams.
    pub(crate) transport: Mutex<Option<Arc<ResponseTransport>>>,
}

impl DispatchState {
    pub(super) fn new(id: u64) -> Arc<Self> {
        Arc::new(Self {
            id,
            subrequests: AtomicU32::new(0),
            log_bytes: AtomicU32::new(0),
            cancellation: Cancellation::default(),
            responded: AtomicBool::new(false),
            cancel_future: AtomicU64::new(0),
            request_body: Mutex::new(None),
            reactor_attribution: crate::reactor_accounting::Attribution::default(),
            transport: Mutex::new(None),
        })
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn set_cancel_future(&self, id: u64) {
        self.cancel_future.store(id, Ordering::Relaxed);
    }

    pub(crate) fn cancel_future(&self) -> u64 {
        self.cancel_future.load(Ordering::Relaxed)
    }

    /// Requests cancellation; the guest sees it from the dispatch's next
    /// slice. Returns false if it was already requested.
    pub(crate) fn request_cancellation(&self) -> bool {
        self.cancellation.request()
    }

    pub(crate) fn cancellation_requested(&self) -> bool {
        self.cancellation.is_requested()
    }

    /// Records that the response is complete. Returns false if it already was.
    pub(crate) fn mark_responded(&self) -> bool {
        !self.responded.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn has_responded(&self) -> bool {
        self.responded.load(Ordering::Acquire)
    }

    /// Once a slice has ended, the cancellation future to complete if the
    /// request has not yet reached the guest.
    pub(crate) fn take_undelivered_cancellation(&self) -> Option<u64> {
        (self.cancellation.take_undelivered() && self.cancel_future() != 0)
            .then(|| self.cancel_future())
    }

    pub(super) fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    pub(crate) fn subrequests(&self) -> u32 {
        self.subrequests.load(Ordering::Relaxed)
    }

    /// Releases the incoming request body.
    pub(super) fn close(&self) {
        self.reactor_attribution.set(None);
        if let Some(body) = self.request_body.lock().take() {
            body.close();
        }
    }
}

impl Drop for DispatchState {
    fn drop(&mut self) {
        self.close();
    }
}
