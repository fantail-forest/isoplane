//! Closing the connections instances keep that nothing uses any more.

use super::*;

impl Cell {
    /// Closes, across every instance, the connections that have carried no
    /// bytes for `idle_connection_ms` and that no dispatch in progress last
    /// used. A connection kept between requests holds host resources and a
    /// slot in the remote service's limits while its instance sleeps, so the
    /// host reclaims it; the guest sees an aborted connection when it next
    /// uses it.
    pub(super) fn close_idle_connections(&self) {
        let slots: Vec<Arc<Slot>> = self
            .index
            .lock()
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        if slots.is_empty() {
            return;
        }
        let idle_before = crate::bindings::reactor_now_on(&self.rt.reactor)
            .checked_sub(Duration::from_millis(self.cfg.idle_connection_ms));
        let Some(idle_before) = idle_before else {
            return;
        };
        for slot in slots {
            let in_progress = self.active_dispatch_ids(slot.id);
            let (closed, wakes) = slot.ctx.close_idle_connections(
                idle_before,
                &in_progress,
                self.cfg.idle_connection_ms,
            );
            for future in wakes {
                self.rt
                    .settle_future(future, isoplane_abi::result::ResultRecord::Ok(Vec::new()));
            }
            if closed > 0 {
                self.metrics
                    .idle_connections_closed
                    .fetch_add(closed as u64, Ordering::Relaxed);
                tracing::debug!(instance = %slot.id, closed, idle_connection_ms = self.cfg.idle_connection_ms,
                    "closed connections that carried nothing");
            }
        }
    }
}
