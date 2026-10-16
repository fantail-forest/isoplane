//! The books for instances killed off the dispatch path, and the
//! panic-recovery paths.

use super::*;

impl Cell {
    /// The books for a guest that trapped or was killed inside the pre-capture
    /// gc hint, on the sweeper's thread with no dispatch to answer. No overage
    /// is claimed: the hint runs on a cap, not a budget the tenant bought.
    pub(super) fn record_offpath_kill(
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
        self.forget(slot);
        tracing::warn!(
            tenant = %entry.tenant, instance = %slot.id, ?completion,
            "the guest was killed in its pre-capture gc hint; the instance is discarded"
        );
    }

    pub(super) fn count_enter_refusal(&self, e: EnterError) {
        match e {
            EnterError::Busy => {
                self.metrics
                    .reentrancy_refusals
                    .fetch_add(1, Ordering::Relaxed);
            }
            // An instance that is not here, counted by the tier gauges.
            EnterError::NotResident(_) => {}
        }
    }

    /// Finalizes only the owner whose slice unwound. Pointer identity
    /// also protects a reused slot when callers repeat dispatch identifiers.
    pub(super) fn answer_after_panic(&self, holding: Option<Arc<DispatchCompletion>>) {
        let Some(owner) = holding else {
            return;
        };
        let _response_settlement;
        let _completion = owner.settlement();
        let Some(id) = owner.instance() else {
            return;
        };
        // One acquisition for the check and the take: `(true, None)` is our
        // own entry already answered, which still needs removing.
        let (mine, tx) = self
            .active
            .with_mut(&id, |a| {
                if Arc::ptr_eq(&a.completion, &owner) {
                    (true, a.responder.take().map(|tx| (tx, a.dispatch_id)))
                } else {
                    (false, None)
                }
            })
            .unwrap_or((false, None));
        if !mine {
            return;
        }
        _response_settlement = self.slot(id).and_then(|slot| {
            slot.ctx.end_http_dispatch();
            let stream = slot.ctx.responses.lock().transport.take();
            stream.map(|stream| {
                stream.fail("host_panic".into());
                stream.settlement()
            })
        });
        self.rt.futures.end_dispatch(id, owner.dispatch_id());
        self.rt.futures.release_instance(id);
        if let Some(slot) = self.slot(id) {
            slot.ctx.request_cancellation();
            slot.discard();
            self.forget(&slot);
        }
        if let Some((tx, _dispatch_id)) = tx {
            owner.answer(&tx, owner.failure_result(500, "host_panic"));
        }
        self.active.remove(&id);
    }
}
