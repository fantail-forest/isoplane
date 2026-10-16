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

use std::sync::atomic::{AtomicBool, Ordering};

/// One dispatch's cancellation. A new dispatch gets a new one, so a request
/// made for an earlier dispatch on the same instance cannot reach it.
#[derive(Default)]
pub(crate) struct Cancellation {
    /// Requested: the dispatch's budget ran out, its client went away, or the
    /// host is ending it.
    requested: AtomicBool,
    /// What the guest sees. Written only by `begin_slice`.
    observed: AtomicBool,
    /// Whether the dispatch's cancellation future has been completed.
    delivered: AtomicBool,
}

impl Cancellation {
    /// Returns false if cancellation was already requested.
    pub(crate) fn request(&self) -> bool {
        !self.requested.swap(true, Ordering::AcqRel)
    }

    /// The only place what the guest observes changes: as the engine enters
    /// the guest for a slice of this dispatch.
    pub(crate) fn begin_slice(&self) {
        self.observed
            .store(self.requested.load(Ordering::Acquire), Ordering::Release);
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
        cancellation.begin_slice();
        assert!(cancellation.request(), "the first request is new");
        assert!(
            !cancellation.observed(),
            "a request made while the guest runs must not change what it sees"
        );
        cancellation.begin_slice();
        assert!(cancellation.observed(), "the next slice sees the request");
    }

    #[test]
    fn a_cancellation_future_is_delivered_once_per_request() {
        let cancellation = Cancellation::default();
        assert!(!cancellation.take_undelivered(), "nothing was requested");
        cancellation.request();
        assert!(cancellation.take_undelivered(), "a request is delivered");
        assert!(!cancellation.take_undelivered(), "and only once");
    }
}
