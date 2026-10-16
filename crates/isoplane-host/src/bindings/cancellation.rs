//! A dispatch's cancellation, kept so the guest can only see it change between
//! slices.
//!
//! SECURITY: guest-visible cancellation stays fixed within a slice so a guest
//! cannot time its code by polling for cancellation. Budget exhaustion and a
//! client disconnect can request cancellation while guest code runs; the guest
//! observes the request at its next slice.
//!
//! So a cancellation is only *requested* when it happens, and the request is
//! never read by anything the guest can reach. What the guest observes is
//! copied from the request in exactly one place, [`Cancellation::begin_slice`],
//! as the engine enters the guest; and the cancellation future that wakes a
//! parked guest is completed only once its slice has ended. Keep every new
//! guest-facing check of cancellation on [`Cancellation::observed`].

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::instance::DispatchGeneration;

#[derive(Default)]
pub(crate) struct Cancellation {
    /// Requested for the current dispatch: its budget ran out, or the host is
    /// ending it.
    requested: AtomicBool,
    /// The newest dispatch generation whose client went away; a later
    /// dispatch on a reused instance is not cancelled by it.
    requested_generation: AtomicU64,
    /// What the guest sees. Written only by `begin_slice`.
    observed: AtomicBool,
    /// Whether the dispatch's cancellation future has been completed.
    delivered: AtomicBool,
}

impl Cancellation {
    /// Requests cancellation of the current dispatch. Returns false if it was
    /// already requested.
    pub(crate) fn request(&self) -> bool {
        !self.requested.swap(true, Ordering::AcqRel)
    }

    /// Requests cancellation of the dispatch of `generation`, if it is still
    /// the current one when the next slice begins.
    pub(crate) fn request_generation(&self, generation: DispatchGeneration) {
        self.requested_generation
            .fetch_max(generation.0, Ordering::AcqRel);
    }

    /// A new dispatch starts uncancelled. Under `reuse` the previous one may
    /// have ended in a clean kill a moment ago.
    pub(crate) fn begin_dispatch(&self) {
        self.requested.store(false, Ordering::Release);
        self.observed.store(false, Ordering::Release);
        self.delivered.store(false, Ordering::Release);
    }

    /// The only place what the guest observes changes: as the engine enters
    /// the guest for the dispatch of `current`.
    pub(crate) fn begin_slice(&self, current: DispatchGeneration) {
        let requested = self.requested.load(Ordering::Acquire)
            || current.0 != 0 && self.requested_generation.load(Ordering::Acquire) >= current.0;
        self.observed.store(requested, Ordering::Release);
    }

    /// Whether the guest's current slice sees the dispatch as cancelled.
    pub(crate) fn observed(&self) -> bool {
        self.observed.load(Ordering::Acquire)
    }

    /// Called once a slice has ended: true exactly once per requested
    /// cancellation, when its future should be completed.
    pub(crate) fn take_undelivered(&self) -> bool {
        self.requested.load(Ordering::Acquire) && !self.delivered.swap(true, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_invisible_to_the_guest_until_the_next_slice_begins() {
        let cancellation = Cancellation::default();
        let current = DispatchGeneration(1);
        cancellation.begin_slice(current);
        assert!(cancellation.request(), "the first request is new");
        assert!(
            !cancellation.observed(),
            "a request made while the guest runs must not change what it sees"
        );
        cancellation.begin_slice(current);
        assert!(cancellation.observed(), "the next slice sees the request");
    }

    #[test]
    fn a_cancellation_future_is_delivered_once_per_request() {
        let cancellation = Cancellation::default();
        assert!(!cancellation.take_undelivered(), "nothing was requested");
        cancellation.request();
        assert!(cancellation.take_undelivered(), "a request is delivered");
        assert!(!cancellation.take_undelivered(), "and only once");
        cancellation.begin_dispatch();
        assert!(
            !cancellation.take_undelivered(),
            "a new dispatch starts with nothing to deliver"
        );
    }

    #[test]
    fn a_delayed_client_disconnect_cannot_cancel_a_later_dispatch() {
        let cancellation = Cancellation::default();
        let (first, second) = (DispatchGeneration(1), DispatchGeneration(2));
        cancellation.request_generation(first);
        cancellation.begin_slice(first);
        assert!(cancellation.observed(), "a disconnect cancels its dispatch");
        cancellation.begin_dispatch();
        cancellation.request_generation(first);
        cancellation.begin_slice(second);
        assert!(
            !cancellation.observed(),
            "an earlier dispatch's disconnect must not cancel a later one"
        );
        cancellation.request_generation(second);
        cancellation.request_generation(first);
        cancellation.begin_slice(second);
        assert!(
            cancellation.observed(),
            "a delayed older disconnect must not undo the current one"
        );
    }
}
