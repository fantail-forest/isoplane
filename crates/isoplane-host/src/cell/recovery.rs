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
        let did = owner.dispatch_id();
        let (mine, tx) = self
            .with_active_mut(id, did, |a| {
                if Arc::ptr_eq(&a.completion, &owner) {
                    (true, a.responder.take())
                } else {
                    (false, None)
                }
            })
            .unwrap_or((false, None));
        if !mine {
            return;
        }
        _response_settlement = self.slot(id).and_then(|slot| {
            let stream = slot
                .ctx
                .dispatch(did)
                .and_then(|dispatch| dispatch.transport.lock().take());
            slot.ctx.end_dispatch(did);
            stream.map(|stream| {
                stream.fail("host_panic".into());
                stream.settlement()
            })
        });
        self.rt.futures.end_dispatch(id, did);
        if let Some(tx) = tx {
            owner.answer(&tx, owner.failure_result(500, "host_panic"));
        }
        self.remove_active(id, did);
        // The panic may have left the instance's host state inconsistent, so
        // the dispatches still on it fail with it.
        if let Some(slot) = self.slot(id) {
            for dispatch in self.active_dispatch_ids(id) {
                self.end_on_stopped_instance(
                    &slot,
                    dispatch,
                    TerminalReason::InstanceDiscarded,
                    503,
                    "instance_discarded",
                );
            }
            slot.discard();
            self.forget(&slot);
        }
        self.rt.futures.release_instance(id);
    }

    /// Answers and ends a dispatch whose instance is going away without a
    /// further slice.
    pub(super) fn end_on_stopped_instance(
        &self,
        slot: &Arc<Slot>,
        dispatch: u64,
        reason: TerminalReason,
        status: u16,
        code: &'static str,
    ) {
        let (active, _) = self.remove_active(slot.id, dispatch);
        let Some(active) = active else {
            return;
        };
        let owner = active.completion;
        owner.select(reason, status);
        let stream = slot
            .ctx
            .dispatch(dispatch)
            .and_then(|state| state.transport.lock().take());
        let _response = stream.as_ref().map(|stream| stream.clone().settlement());
        let _completion = owner.settlement();
        if let Some(stream) = stream {
            stream.fail(code.into());
        }
        if let Some(state) = slot.ctx.dispatch(dispatch) {
            state.request_cancellation();
        }
        slot.ctx.end_dispatch(dispatch);
        self.rt.futures.end_dispatch(slot.id, dispatch);
        if let Some(sender) = active.responder {
            owner.answer(&sender, owner.failure_result(status, code));
        }
    }
}
