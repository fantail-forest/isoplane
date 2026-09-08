//! Discards a selected resident instance.

use super::*;

impl Cell {
    /// Discards an instance and releases its host futures without saving guest memory.
    pub fn discard_instance(self: &Arc<Self>, id: InstanceId) -> crate::Result<()> {
        let slot = self
            .slot(id)
            .ok_or_else(|| anyhow::anyhow!("no such instance {id}"))?;
        let _execution = slot.try_lock_execution().ok_or_else(|| {
            anyhow::anyhow!("instance {id} is executing; eviction requires an idle boundary")
        })?;
        if slot.state() == LifecycleTier::Hot {
            anyhow::bail!("instance {id} is Hot; eviction is from Parked, not from a live stack");
        }
        self.rt.futures.release_instance(id);
        self.forget(&slot);
        slot.discard();
        Ok(())
    }
}
