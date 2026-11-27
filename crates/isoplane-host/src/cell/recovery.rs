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
        if completion != Completion::BudgetKill {
            self.metrics.traps.fetch_add(1, Ordering::Relaxed);
        }
        #[cfg(any(test, feature = "test-util"))]
        if completion == Completion::BudgetKill {
            entry
                .meters
                .record_grace_end(isoplane_engine::GraceEnd::Killed, 0);
            self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
        }
        self.forget(slot);
        tracing::warn!(
            tenant = %entry.tenant, instance = %slot.id, ?completion,
            "the guest was killed in its pre-capture gc hint; the instance is discarded"
        );
    }

    /// Counts and reports a lifecycle transition a slot refused.
    pub(super) fn note_refused_transition(&self, tenant: &str, refused: TransitionError) {
        self.metrics
            .refused_transitions
            .fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            cell = %self.cfg.name, tenant, instance = %refused.instance,
            from = %refused.from, to = %refused.to,
            "an instance refused a lifecycle transition"
        );
    }

    /// Returns an instance to its pool warm. An instance the pool could not
    /// park was discarded there, so the cell lets go of it as well.
    pub(super) fn release_to_pool(&self, entry: &ModuleEntry, slot: &Arc<Slot>) {
        if let Err(refused) = entry.pool.release(slot.clone(), warm()) {
            self.note_refused_transition(&entry.tenant, refused);
            self.rt.futures.release_instance(slot.id);
            self.forget(slot);
        }
    }

    /// Ends an instance that refused to park after a slice: the dispatches
    /// still on it and the starts queued for it fail, and it leaves the cell
    /// and its pool.
    pub(super) fn end_unparkable_instance(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        refused: TransitionError,
    ) {
        self.note_refused_transition(&entry.tenant, refused);
        self.end_instance(
            slot,
            TerminalReason::InstanceDiscarded,
            503,
            "instance_discarded",
        );
    }

    /// Refuses every start queued for an instance that has ended, each
    /// giving up the place it held there. A queued start is otherwise begun
    /// by a wake, and an ended instance is not woken again.
    pub(super) fn refuse_queued_starts(&self, instance: InstanceId) {
        for (queued, job) in self.starts.remove(&instance).unwrap_or_default() {
            let owner = job.completion.clone();
            self.waited_for_thread(&owner, queued.elapsed());
            owner.in_scope(|| self.refuse_joined_start(instance, job));
        }
    }

    /// Answers a dispatch that reserved a place on an instance which ended
    /// before the dispatch began there, and gives the place up.
    pub(super) fn refuse_joined_start(&self, instance: InstanceId, job: DispatchJob) {
        self.release_reservation(instance);
        job.completion
            .select(TerminalReason::InstanceDiscarded, 503);
        let _completion = job.completion.settlement();
        job.completion.answer(
            &job.responder,
            DispatchResult {
                status: 503,
                body: b"instance_discarded".to_vec(),
                ..DispatchResult::shed(job.rec.dispatch_id)
            },
        );
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
        // Answered last, so a caller never sees its instance still resident.
        if let Some(tx) = tx {
            owner.answer(&tx, owner.failure_result(500, "host_panic"));
        }
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
        let Some(active) = self.remove_active(slot.id, dispatch).dispatch else {
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
