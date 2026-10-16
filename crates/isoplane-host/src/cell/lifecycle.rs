//! One dispatch from admission to its final response: the dispatch lifecycle across every slice, the outcome switch, and the books a killed instance keeps.

use super::*;

impl Cell {
    pub(super) fn run_dispatch(self: &Arc<Self>, job: DispatchJob) {
        let owner = job.completion.clone();
        owner.in_scope(|| self.run_owned_dispatch(job));
    }

    fn run_owned_dispatch(self: &Arc<Self>, job: DispatchJob) {
        IN_FLIGHT.replace(Some(Arc::downgrade(&job.completion)));
        let did = job.rec.dispatch_id;
        let entry = job.completion.entry.clone();

        let t0 = Instant::now();
        let acquired = tracing::info_span!("acquire").in_scope(|| match &job.target {
            Delivery::Handler { .. } => self.acquire_handler(&entry),
            Delivery::IdleTimers { slot, .. } => Ok((slot.clone(), AcquireSource::WarmParked)),
        });
        let (slot, src) = match acquired {
            Ok(v) => v,
            Err(e) => {
                let status = job.completion.acquisition_failed(&e);
                job.completion.finish();
                job.completion.answer(
                    &job.responder,
                    DispatchResult {
                        status,
                        body: if status == 503 {
                            b"snapshot_capacity_unavailable".to_vec()
                        } else {
                            e.to_string().into_bytes()
                        },
                        ..DispatchResult::shed(did)
                    },
                );
                return;
            }
        };
        let _execution = slot.lock_execution();
        self.note_acquired(src, t0.elapsed());
        let acquired = src.label();
        job.completion.acquired(slot.id, acquired);

        let budget = OccupancyBudget {
            occupancy_ms: job.rec.occupancy_budget_ms,
            slice_wall_ms_cap: self.cfg.slice_wall_ms_cap,
            // Later slices carry what earlier ones spent, so the budget binds
            // the dispatch rather than each wake.
            spent_ns: 0,
        };
        // The cancellation future is allocated before the guest is entered,
        // because the epoch callback that completes it runs with the guest on
        // the stack.
        let mut job = job;
        job.rec.cancel_future = self
            .rt
            .futures
            .alloc_cancellation(slot.id, job.rec.dispatch_id);

        self.insert_active(
            slot.id,
            ActiveDispatch {
                dispatch_id: job.rec.dispatch_id,
                module: job.module.clone(),
                route: job.target.route(),
                idle_stop_at: match &job.target {
                    Delivery::Handler { .. } => None,
                    Delivery::IdleTimers { stop_at, .. } => Some(*stop_at),
                },
                responder: Some(job.responder),
                completion: job.completion.clone(),
            },
        );

        entry.meters.record_dispatch();
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);

        let dispatch = slot
            .ctx
            .start_dispatch(crate::now_ms(), job.rec.dispatch_id);
        let observer: Arc<dyn crate::reactor_accounting::Observer> = job.completion.clone();
        *dispatch.reactor_observer.lock() = Some(Arc::downgrade(&observer));
        dispatch.set_cancel_future(job.rec.cancel_future);
        if let isoplane_abi::dispatch::DispatchKind::Http(request) = &job.rec.kind {
            *dispatch.request_body.lock() = Some(job.upload.clone().unwrap_or_else(|| {
                crate::bindings::request_body::RequestBodyScope::new(None, !request.body.is_empty())
            }));
        }

        *dispatch.transport.lock() = job.stream.clone();
        if let Some(stream) = &job.stream {
            let cell = Arc::downgrade(self);
            let owner = Arc::downgrade(&slot);
            let cancelled = Arc::downgrade(&dispatch);
            let response = Arc::downgrade(stream);
            stream.on_end_requested(Box::new(move || {
                if let (Some(cell), Some(slot), Some(response)) =
                    (cell.upgrade(), owner.upgrade(), response.upgrade())
                {
                    if response.is_cancelled() || response.failure().is_some() {
                        if let Some(dispatch) = cancelled.upgrade() {
                            dispatch.request_cancellation();
                        }
                    }
                    cell.queue_wake(slot);
                }
            }));
        }
        drop(dispatch);

        if self.end_stream_at_boundary(&slot, &entry, did, None) {
            return;
        }

        let outcome = {
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(e) => {
                    self.count_enter_refusal(e);
                    job.completion.select(TerminalReason::EntryRefused, 409);
                    let _completion = job.completion.settlement();
                    self.respond(
                        &slot,
                        did,
                        DispatchResult {
                            status: 409,
                            body: b"instance busy".to_vec(),
                            ..DispatchResult::shed(did)
                        },
                    );
                    let _settlement = slot
                        .ctx
                        .dispatch(did)
                        .and_then(|dispatch| dispatch.transport.lock().take())
                        .map(|stream| stream.settlement());
                    slot.ctx.end_dispatch(did);
                    self.rt.futures.end_dispatch(slot.id, did);
                    self.remove_active(slot.id, did);
                    return;
                }
            };
            let hot = super::completion::HotSlice::new(&self.hot);
            job.completion.begin_slice();
            let out = tracing::info_span!("guest_slice")
                .in_scope(|| guard.instance().dispatch(&job.rec, budget));
            drop(hot);
            job.completion.add_slice(&out);
            entry.meters.record_occupancy(&out);
            if ends_the_instance(&out, &slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            out
        };

        // A completion that landed while the dispatch was Hot waited on the
        // reentrancy rule; hand the instance to the wake lane for it.
        if !self.settle(&slot, &entry, did, outcome, &job.completion) {
            self.queue_wake(slot.clone());
        }
    }

    /// The metrics for how a dispatch got its instance.
    fn note_acquired(&self, src: AcquireSource, took: Duration) {
        match src {
            AcquireSource::WarmParked => &self.metrics.served_warm,
            AcquireSource::PooledFresh => &self.metrics.served_pooled_fresh,
            AcquireSource::HibernationWake => {
                self.metrics
                    .hibernation_wake_us
                    .record(took.as_micros() as u64);
                self.metrics
                    .hibernation_wakes
                    .fetch_add(1, Ordering::Relaxed);
                &self.metrics.served_hibernation_wake
            }
            AcquireSource::Activated => {
                self.metrics.activation_us.record(took.as_micros() as u64);
                &self.metrics.served_activated
            }
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn queue_wake(&self, slot: Arc<Slot>) {
        self.metrics.in_cell_wakes.fetch_add(1, Ordering::Relaxed);
        self.queue.push_wake(WakeItem { slot });
    }

    /// One woken slice, and every further slice the instance can take without
    /// going back through a lane. Each slice runs the dispatch that owns the
    /// future it delivers.
    pub(super) fn run_wake(self: &Arc<Self>, slot: Arc<Slot>) {
        let _execution = slot.lock_execution();
        let Some(entry) = self.first_active(slot.id, |dispatch| dispatch.completion.entry.clone())
        else {
            return;
        };
        for dispatch in self.active_dispatch_ids(slot.id) {
            if self.end_stream_at_boundary(&slot, &entry, dispatch, None)
                && !self.active.contains_key(&slot.id)
            {
                return;
            }
        }
        if slot.state() == LifecycleTier::Hibernated && !self.restore_for_wake(&slot, &entry) {
            return;
        }

        loop {
            // The instance is entered before the result is taken out, so a
            // result is never pulled with nowhere to deliver it. A busy
            // instance drains the result itself when its call returns.
            let guard = match slot.enter() {
                Ok(g) => g,
                Err(EnterError::Busy) => {
                    self.count_enter_refusal(EnterError::Busy);
                    return;
                }
                Err(error @ EnterError::NotResident(_)) => {
                    self.count_enter_refusal(error);
                    self.fail_instance(
                        &slot,
                        &entry,
                        TerminalReason::EntryRefused,
                        409,
                        "instance_unavailable",
                    );
                    return;
                }
            };
            let Some(ready) = self.rt.futures.take_ready(slot.id) else {
                // The first wake's loop drained every completion.
                return;
            };
            let Some(owner) = self.with_active(slot.id, ready.dispatch, |dispatch| {
                dispatch.completion.clone()
            }) else {
                tracing::debug!(instance = %slot.id, dispatch = ready.dispatch,
                    "dropped a result for a dispatch that has ended");
                continue;
            };
            IN_FLIGHT.replace(Some(Arc::downgrade(&owner)));
            let done = owner.in_scope(|| self.run_woken_slice(&slot, &entry, guard, ready, &owner));
            if done {
                return;
            }
        }
    }

    /// Restores a hibernated instance before its next slice; false if it
    /// could not be restored, in which case every dispatch on it has failed.
    fn restore_for_wake(self: &Arc<Self>, slot: &Arc<Slot>, entry: &Arc<ModuleEntry>) -> bool {
        let started = Instant::now();
        if let Err(error) = self.wake(slot, entry) {
            let message = format!(
                "instance {} could not restore its pending dispatches: {error}",
                slot.id
            );
            for dispatch in self.active_dispatch_ids(slot.id) {
                let Some(owner) =
                    self.with_active(slot.id, dispatch, |active| active.completion.clone())
                else {
                    continue;
                };
                owner.select(TerminalReason::RestoreFailed, 500);
                let result = owner.result(
                    slot.id,
                    500,
                    vec![],
                    message.as_bytes().to_vec(),
                    Completion::Trapped,
                    Some(message.clone()),
                );
                self.respond(slot, dispatch, result);
            }
            self.fail_instance(
                slot,
                entry,
                TerminalReason::RestoreFailed,
                500,
                "restore_failed",
            );
            return false;
        }
        self.metrics
            .hibernation_wake_us
            .record(started.elapsed().as_micros() as u64);
        self.metrics
            .hibernation_wakes
            .fetch_add(1, Ordering::Relaxed);
        entry.meters.record_hibernation_wake();
        true
    }

    /// Delivers one ready result in a slice of its dispatch. Returns true when
    /// the instance takes no more wakes in this loop.
    fn run_woken_slice(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        mut guard: crate::slot::CallGuard<'_>,
        ready: crate::futures::Ready,
        owner: &Arc<DispatchCompletion>,
    ) -> bool {
        // The budget binds the dispatch, so a wake resumes with what earlier
        // slices burned; otherwise a guest could park and wake indefinitely on
        // a fresh budget each time.
        let budget = owner.next_budget();
        slot.ctx.begin_slice(crate::now_ms(), ready.dispatch);

        let t0 = Instant::now();
        let hot = super::completion::HotSlice::new(&self.hot);
        owner.begin_slice();
        let outcome = tracing::info_span!("guest_slice").in_scope(|| {
            guard
                .instance()
                .wake(ready.dispatch, ready.future, &ready.result, budget)
        });
        drop(hot);
        owner.add_slice(&outcome);
        entry.meters.record_occupancy(&outcome);
        if ends_the_instance(&outcome, slot) {
            guard.destroy();
        } else {
            drop(guard);
        }
        self.metrics.wake_us.record(t0.elapsed().as_micros() as u64);

        self.settle(slot, entry, ready.dispatch, outcome, owner)
    }

    /// Returns true when the instance takes no more wakes in this loop.
    fn settle(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        did: u64,
        out: DispatchOutcome,
        owner: &Arc<DispatchCompletion>,
    ) -> bool {
        if out.grace_deferrals > 0 {
            self.metrics
                .grace_deferrals
                .fetch_add(out.grace_deferrals, Ordering::Relaxed);
        }
        self.metrics.record_tenant(
            &entry.tenant,
            out.occupancy_ns,
            !matches!(out.completion, Completion::Done | Completion::Parked),
        );

        // A latched trap becomes a trap here, before the guest proceeds as
        // though the import had succeeded. A clean kill is left alone: its
        // latched code is almost always a poisoned import the guest ignored
        // on its way out, and its slot is consistent.
        let latched = slot.ctx.take_trap();
        let mut latched_msg = None;
        let completion = match latched {
            Some(code) if matches!(out.completion, Completion::Done | Completion::Parked) => {
                if code == TrapCode::TooManyFutures {
                    self.metrics
                        .pending_cap_rejections
                        .fetch_add(1, Ordering::Relaxed);
                }
                owner.host_trap(code);
                latched_msg = Some(format!("{code:?}"));
                Completion::Trapped
            }
            _ => out.completion,
        };

        let closed_response = slot
            .ctx
            .dispatch(did)
            .filter(|dispatch| dispatch.transport.lock().is_some())
            .and_then(|_| slot.ctx.responses.lock().finished_stream(did));
        let completion = if completion == Completion::Parked && closed_response.is_some() {
            Completion::Done
        } else {
            completion
        };

        let (module, route, runs_idle_timers) = self
            .with_active(slot.id, did, |req| {
                (
                    req.module.clone(),
                    req.route.clone(),
                    req.idle_stop_at.is_some(),
                )
            })
            .unwrap_or_else(|| (entry.activator.module().clone(), String::new(), false));
        // Running timers has no result to wait for: it is done once the guest
        // waits on nothing but more timers.
        let completion = if completion == Completion::Parked
            && runs_idle_timers
            && !self.rt.futures.holds_guest_work(slot.id)
        {
            Completion::Done
        } else {
            completion
        };
        let breaker_key = BreakerKey { module, route };
        if self.end_stream_at_boundary(slot, entry, did, Some(completion)) {
            return true;
        }

        match completion {
            Completion::Parked => self.park_after_slice(slot),
            Completion::Done => {
                // Open response handles retain the active dispatch. A streaming
                // caller consumes the transport; buffered callers keep their
                // existing snapshot reply semantics.
                let response_handle = match out.status {
                    PollStatus::Done { response } if response != 0 => Some(response),
                    _ => closed_response,
                };
                let response = match response_handle {
                    Some(response) => {
                        let mut t = slot.ctx.responses.lock();
                        match t.get(response) {
                            Some(r) if r.head.streaming && !r.finished => Some(r.clone()),
                            _ => t.take(response),
                        }
                    }
                    _ => None,
                };
                let streaming_open = response
                    .as_ref()
                    .is_some_and(|r| r.head.streaming && !r.finished);

                let (status, headers, body) = match &response {
                    Some(r) => (r.head.status, r.head.headers.clone(), r.body()),
                    None => (204, vec![], Vec::new()),
                };
                let result = owner.result(slot.id, status, headers, body, completion, None);
                let streams = slot
                    .ctx
                    .dispatch(did)
                    .is_some_and(|dispatch| dispatch.transport.lock().is_some());
                if !streaming_open || !streams {
                    self.respond(slot, did, result);
                }

                match decide(completion, streaming_open) {
                    DispatchEnd::RetainedForStreaming => {
                        slot.park(crate::now_ms());
                        !self.rt.futures.has_ready(slot.id)
                    }
                    DispatchEnd::Disposed(d) => {
                        self.breaker.record_success(&breaker_key);
                        self.finish_and_release(slot, entry, did, d)
                    }
                    DispatchEnd::Parked => self.park_after_slice(slot),
                }
            }
            // The dispatch still fails with `budget_exceeded`, but the
            // instance is consistent, its slot is reused, and the breaker
            // records neither a kill nor a success: a clean kill is the budget
            // working as designed.
            Completion::CleanKill => {
                let overage = out.overage_ns;
                entry.meters.record_budget_kill(KillPhase::Clean, overage);
                self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
                self.metrics.clean_kills.fetch_add(1, Ordering::Relaxed);
                self.metrics.grace_entries.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .overage_ns
                    .fetch_add(overage, Ordering::Relaxed);
                tracing::info!(
                    tenant = %entry.tenant, instance = %slot.id,
                    overage_us = overage / 1_000,
                    "clean kill: dispatch unwound inside its grace window"
                );
                let msg = format!(
                    "budget_exceeded: occupancy budget elapsed; the dispatch unwound cleanly inside its \
                     grace window with {:.3} ms of recorded overage",
                    overage as f64 / 1e6
                );
                let result = owner.result(
                    slot.id,
                    503,
                    vec![],
                    msg.clone().into_bytes(),
                    completion,
                    Some(msg),
                );
                self.respond(slot, did, result);
                match decide(completion, false) {
                    DispatchEnd::Disposed(d) => self.finish_and_release(slot, entry, did, d),
                    DispatchEnd::Parked | DispatchEnd::RetainedForStreaming => {
                        self.park_after_slice(slot)
                    }
                }
            }
            Completion::Trapped | Completion::BudgetKill => {
                let killed = completion == Completion::BudgetKill;
                if killed {
                    entry
                        .meters
                        .record_budget_kill(KillPhase::Hard, out.overage_ns);
                    self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
                    self.metrics.hard_kills.fetch_add(1, Ordering::Relaxed);
                    self.metrics
                        .overage_ns
                        .fetch_add(out.overage_ns, Ordering::Relaxed);
                    if out.kill.is_some() {
                        self.metrics.grace_entries.fetch_add(1, Ordering::Relaxed);
                    }
                    self.breaker.record_kill(&breaker_key, crate::now_ms());
                } else {
                    entry.meters.record_trap();
                    // A hard kill is not also a trap.
                    self.metrics.traps.fetch_add(1, Ordering::Relaxed);
                }
                // Refuses entry from here; `finish_and_release` settles the
                // pool's books.
                slot.discard();
                let msg = out
                    .trap
                    .as_ref()
                    .map(|t| format!("{}: {}", t.kind.tenant_code(), t.message))
                    .or_else(|| latched_msg.map(|c| format!("capability_violation: {c}")))
                    .unwrap_or_else(|| "trap".into());
                let status = if killed { 503 } else { 500 };
                let result = owner.result(
                    slot.id,
                    status,
                    vec![],
                    msg.clone().into_bytes(),
                    completion,
                    Some(msg),
                );
                self.respond(slot, did, result);
                self.finish_and_release(slot, entry, did, evict())
            }
        }
    }

    /// The instance parks between slices. Returns whether the wake loop is
    /// done with it.
    fn park_after_slice(&self, slot: &Arc<Slot>) -> bool {
        slot.park(crate::now_ms());
        // A completion that landed while Hot is drained now.
        !self.rt.futures.has_ready(slot.id)
    }

    fn respond(&self, slot: &Arc<Slot>, did: u64, result: DispatchResult) {
        self.with_active(slot.id, did, |dispatch| {
            dispatch.completion.select_result(&result)
        });
        let stream = slot
            .ctx
            .dispatch(did)
            .and_then(|dispatch| dispatch.transport.lock().clone());
        if let Some(stream) = stream {
            if stream.has_started() {
                if let Some(error) = &result.trap {
                    stream.fail(error.clone());
                } else {
                    stream.finish();
                }
            }
        }
        let response = self
            .with_active_mut(slot.id, did, |dispatch| {
                dispatch
                    .responder
                    .take()
                    .map(|sender| (dispatch.completion.clone(), sender))
            })
            .flatten();
        if let Some((owner, sender)) = response {
            owner.answer(&sender, result);
        }
    }

    fn end_stream_at_boundary(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        did: u64,
        completion: Option<Completion>,
    ) -> bool {
        let Some(owner) = self.with_active(slot.id, did, |dispatch| dispatch.completion.clone())
        else {
            return false;
        };
        let Some(dispatch) = slot.ctx.dispatch(did) else {
            return false;
        };
        let Some(stream) = dispatch.transport.lock().clone() else {
            return false;
        };
        let failure = stream.failure();
        if !stream.is_cancelled() && failure.is_none() {
            let Some((status, headers)) = stream.no_body_head() else {
                return false;
            };
            if completion.is_some_and(|completion| {
                !matches!(completion, Completion::Done | Completion::Parked)
            }) {
                return false;
            }
            dispatch.request_cancellation();
            let result = owner.result(slot.id, status, headers, vec![], Completion::Done, None);
            self.respond(slot, did, result);
            self.finish_and_release(slot, entry, did, evict());
            return true;
        }
        dispatch.request_cancellation();
        let status = if failure.is_some() { 502 } else { 499 };
        let terminal = if failure.is_some() {
            TerminalReason::TransportFailure
        } else {
            TerminalReason::RequestCancelled
        };
        owner.select(terminal, status);
        let reason = failure.unwrap_or_else(|| "request_cancelled".into());
        let result = owner.result(
            slot.id,
            status,
            vec![],
            vec![],
            Completion::Trapped,
            Some(reason),
        );
        self.respond(slot, did, result);
        self.finish_and_release(slot, entry, did, evict());
        true
    }

    /// Fails every dispatch in progress on an instance that cannot continue,
    /// then evicts it.
    pub(super) fn fail_instance(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        reason: TerminalReason,
        status: u16,
        code: &'static str,
    ) {
        let dispatches = self.active_dispatch_ids(slot.id);
        let Some((&last, others)) = dispatches.split_last() else {
            return;
        };
        for &dispatch in others {
            self.fail_dispatch(slot, dispatch, reason, status, code);
            self.end_dispatch_books(slot, entry, dispatch);
        }
        self.fail_dispatch(slot, last, reason, status, code);
        self.finish_and_release(slot, entry, last, evict());
    }

    fn fail_dispatch(
        &self,
        slot: &Arc<Slot>,
        dispatch: u64,
        reason: TerminalReason,
        status: u16,
        code: &'static str,
    ) {
        let Some(owner) = self.with_active(slot.id, dispatch, |active| active.completion.clone())
        else {
            return;
        };
        owner.select(reason, status);
        let result = owner.result(
            slot.id,
            status,
            vec![],
            code.as_bytes().to_vec(),
            Completion::Trapped,
            Some(code.into()),
        );
        self.respond(slot, dispatch, result);
    }

    /// Ends one dispatch's books without releasing the instance: its
    /// response transport, completion event, host state and futures.
    /// Returns the ended dispatch, how many remain on the instance, and
    /// whether the dispatch left guest work behind.
    fn end_dispatch_books(
        &self,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        did: u64,
    ) -> (Option<ActiveDispatch>, usize, bool) {
        let _settlement = slot
            .ctx
            .dispatch(did)
            .and_then(|dispatch| dispatch.transport.lock().take())
            .map(|stream| stream.settlement());
        let (req, remaining) = self.remove_active(slot.id, did);
        let _completion = req.as_ref().map(|owner| owner.completion.settlement());
        slot.ctx.end_dispatch(did);
        slot.ctx.responses.lock().end_dispatch(did);
        let mut abandoned_guest_work = false;
        if let Some(r) = &req {
            abandoned_guest_work = self.rt.futures.end_dispatch(slot.id, did);
            if let Some(total_wall_ns) = r.completion.total_wall_ns() {
                self.metrics.dispatch_us.record(total_wall_ns / 1_000);
            }
            self.metrics.park_ms.record(slot.parked_ms_total());
            entry.meters.record_park(slot.parked_ms_total());
        }
        (req, remaining, abandoned_guest_work)
    }

    /// Ends a dispatch and, once none remain on the instance, releases the
    /// instance with disposition `d`. A disposition that loses the instance
    /// fails the dispatches still in progress on it. Returns true: the wake
    /// loop is done with the instance.
    fn finish_and_release(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        did: u64,
        d: crate::disposition::Disposition,
    ) -> bool {
        let generation = slot.ctx.dispatch_generation();
        let (req, remaining, abandoned_guest_work) = self.end_dispatch_books(slot, entry, did);
        let d = if abandoned_guest_work { evict() } else { d };
        let keeps_instance =
            d.is_warm() || matches!(d.kind(), crate::disposition::DispositionKind::Reset);
        if remaining > 0 {
            if keeps_instance {
                return self.park_after_slice(slot);
            }
            for dispatch in self.active_dispatch_ids(slot.id) {
                self.fail_dispatch(
                    slot,
                    dispatch,
                    TerminalReason::InstanceDiscarded,
                    503,
                    "instance_discarded",
                );
                self.end_dispatch_books(slot, entry, dispatch);
            }
        }

        if !keeps_instance {
            self.forget(slot);
        }
        self.rt.futures.release_instance(slot.id);
        let warm = d.is_warm();
        entry.pool.release(slot.clone(), d);
        if slot.state() == LifecycleTier::Discarded {
            self.forget(slot);
        } else if warm {
            self.watch_idle_timers(
                slot,
                entry,
                generation,
                req.and_then(|dispatch| dispatch.idle_stop_at),
            );
        }
        true
    }
}
