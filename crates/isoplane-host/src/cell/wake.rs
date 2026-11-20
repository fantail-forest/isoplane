//! The wake sink: routes a completed host future onto the execution pool.

use super::*;

/// Routes completions onto the exec pool. Weak references, so a discarded
/// instance's late completion is a no-op rather than a resurrection.
pub(super) struct CellWake {
    pub(super) queue: Arc<WorkQueue>,
    pub(super) index: Arc<Mutex<HashMap<InstanceId, Weak<Slot>>>>,
}

impl WakeSink for CellWake {
    fn signal(&self, owner: InstanceId) {
        let slot = self.index.lock().get(&owner).and_then(Weak::upgrade);
        let Some(s) = slot else {
            // The result is already Ready in the future table; for a live
            // instance this is a dispatch that is never answered.

            tracing::debug!(
                instance = %owner,
                "a completion arrived for an instance that is no longer resident; the wake \
                 is dropped and any dispatch awaiting it will not be answered"
            );
            return;
        };

        self.queue.push_wake(WakeItem { slot: s });
    }
}
