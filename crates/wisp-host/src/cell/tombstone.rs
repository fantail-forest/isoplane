//! The tombstone ledger and the panic-recovery paths that consult it.

use super::*;

impl Cell {
    /// The books for a guest that trapped or was killed inside the pre-capture
    /// gc hint, on the sweeper's thread with no request to answer. No overage
    /// is claimed: the hint runs on a cap, not a budget the tenant bought.
    pub(super) fn record_offpath_tombstone(
        &self,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        completion: Completion,
    ) {
        if completion == Completion::BudgetKill {
            entry.meters.record_budget_kill(KillPhase::Hard, 0);
            self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
            self.metrics.hard_kills.fetch_add(1, Ordering::Relaxed);
        } else {
            entry.meters.record_trap();
            self.metrics.traps.fetch_add(1, Ordering::Relaxed);
        }
        self.metrics.tombstones.fetch_add(1, Ordering::Relaxed);
        self.remember_tombstone(slot);
        self.forget(slot);
        tracing::warn!(
            tenant = %entry.tenant, instance = %slot.id, ?completion,
            "the guest ended its pre-capture gc-hint in a tombstone; the instance is \
             discarded and the slot keeps refusing"
        );
    }

    pub(super) fn remember_tombstone(&self, slot: &Arc<Slot>) {
        /// Covers the burst a breaker takes to notice.
        const RING: usize = 256;
        let mut g = self.tombstoned.lock();
        if g.iter().any(|s| s.id == slot.id) {
            return;
        }
        g.push_back(slot.clone());
        while g.len() > RING {
            g.pop_front();
        }
    }

    pub fn tombstoned_instances(&self) -> Vec<InstanceId> {
        self.tombstoned.lock().iter().map(|s| s.id).collect()
    }

    pub(super) fn count_enter_refusal(&self, e: EnterError) {
        match e {
            EnterError::Busy => {
                self.metrics
                    .reentrancy_refusals
                    .fetch_add(1, Ordering::Relaxed);
            }
            EnterError::Tombstoned => {
                self.metrics
                    .tombstone_refusals
                    .fetch_add(1, Ordering::Relaxed);
            }
            // An instance that is not here, counted by the tier gauges.
            EnterError::NotResident(_) => {}
        }
    }

    /// A call into an instance through the same door every dispatch uses,
    /// with the refusal counted. A killed instance refuses as a tombstone
    /// rather than reading as "no such instance".
    pub fn with_instance<R>(
        &self,
        id: InstanceId,
        f: impl FnOnce(&mut (dyn wisp_engine::InstanceOps + Send)) -> R,
    ) -> Result<R, EnterError> {
        let Some(slot) = self.slot(id) else {
            return Err(EnterError::NotResident(LifecycleTier::Discarded));
        };
        // Bound, because the temporary borrows `slot`.
        let out = match slot.enter() {
            Ok(mut guard) => Ok(f(guard.instance())),
            Err(e) => {
                self.count_enter_refusal(e);
                Err(e)
            }
        };
        out
    }

    /// Answers the caller of a panicked dispatch with a 500, if the request in
    /// `active` is still the one this thread was running; a mismatch means it
    /// was already cleaned up. The slot's lifecycle is left alone, because
    /// nothing here knows what state the panic left.
    pub(super) fn answer_after_panic(&self, holding: Option<(InstanceId, u64)>) {
        let Some((id, dispatch_id)) = holding else {
            return;
        };
        // One acquisition for the check and the take: `(true, None)` is our
        // own entry already answered, which still needs removing.
        let (mine, tx) = self
            .active
            .with_mut(&id, |a| {
                if a.dispatch_id == dispatch_id {
                    (true, a.responder.take())
                } else {
                    (false, None)
                }
            })
            .unwrap_or((false, None));
        if !mine {
            return;
        }
        let _settlement = self.slot(id).and_then(|slot| {
            slot.ctx.end_http_dispatch();
            let stream = slot.ctx.responses.lock().transport.take();
            stream.map(|stream| {
                stream.fail("host_panic".into());
                stream.settlement()
            })
        });
        if let Some(tx) = tx {
            answer(
                &tx,
                DispatchResult {
                    status: 500,
                    body: b"host fault: a panic was caught inside this dispatch".to_vec(),
                    trap: Some("host_panic".into()),
                    instance: id,
                    ..DispatchResult::shed(dispatch_id)
                },
            );
        }
        self.active.remove(&id);
    }
}
