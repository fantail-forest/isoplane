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

    /// Whether cancellation has been requested, for the host. The guest reads
    /// [`Cancellation::observed`] instead.
    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// The only place what the guest observes changes: as the engine enters
    /// the guest for a slice of this dispatch.
    ///
    /// SECURITY: a guest that saw cancellation change within a slice could
    /// time its code by polling for it.
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
