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
        let mut held = false;
        if matches!(job.target, Delivery::Handler { .. }) {
            match self.join_shared(&entry, false) {
                registry::Join::Unavailable { held: h } => held = h,
                registry::Join::Joined(slot) => {
                    self.start_shared(&slot, &entry, job);
                    return;
                }
            }
        }
        let acquired = tracing::info_span!("acquire").in_scope(|| match &job.target {
            Delivery::Handler { .. } => self.acquire_handler(&entry),
        });
        let (slot, src) = match acquired {
            Ok(v) => v,
            Err(e) => {
                if let registry::Join::Joined(slot) = self.join_shared(&entry, true) {
                    self.start_shared(&slot, &entry, job);
                    return;
                }
                let status = job.completion.acquisition_failed(&e);
                job.completion.finish();
                job.completion.answer(
                    &job.responder,
                    DispatchResult {
                        status,
                        body: if e.is::<crate::admission::InstanceLimitReached>() {
                            b"instance_capacity_unavailable".to_vec()
                        } else if status == 503 {
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
        if held && src == AcquireSource::Activated {
            #[cfg(any(test, feature = "test-util"))]
            {
                self.metrics
                    .activations_for_holds
                    .fetch_add(1, Ordering::Relaxed);
                entry.meters.record_activation_for_hold();
            }
            job.completion.activated_for_hold();
        }
        self.begin_dispatch(&slot, &entry, job, src);
    }

    fn start_shared(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        job: DispatchJob,
    ) {
        match slot.try_lock_execution() {
            Some(_execution) => self.begin_dispatch(slot, entry, job, AcquireSource::Shared),
            None => {
                self.starts
                    .upsert(slot.id, |starts| starts.push_back((Instant::now(), job)));
                self.queue_wake(slot.clone());
            }
        }
    }

    /// Begins a dispatch on `slot` and runs its first slice; the caller holds
    /// the instance's execution lock. A dispatch that joined an instance
    /// already running others ([`AcquireSource::Shared`]) holds a reserved
    /// place there until now.
    fn begin_dispatch(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        job: DispatchJob,
        src: AcquireSource,
    ) {
        let did = job.rec.dispatch_id;
        let joined = src == AcquireSource::Shared;
        if joined && !self.ready_to_join(slot, entry) {
            self.refuse_joined_start(slot.id, job);
            self.refuse_queued_starts(slot.id);
            return;
        }
        if joined && !slot.has_memory_for_join() {
            if self.release_reservation(slot.id) == 0 {
                if slot.is_retiring() {
                    self.evict_instance(slot);
                } else {
                    self.release_to_pool(entry, slot);
                    if slot.state() != LifecycleTier::Discarded {
                        self.watch_idle_timers(slot, entry);
                    }
                }
            }
            self.queue_dispatch(Box::new(job));
            return;
        }
        if joined {
            self.note_acquired(AcquireSource::Shared, Duration::ZERO);
        }
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

                grace_deadline: None,
                responder: Some(job.responder),
                completion: job.completion.clone(),
            },
            joined,
        );
        // Offered only once the dispatch is tracked: a dispatch that tried to
        // join before then would find the instance idle and drop it. A local
        // debugger instance runs initialization inside its one dispatch, and
        // accepts no other.
        if matches!(job.target, Delivery::Handler { .. }) && entry.debug_occupancy_ms.is_none() {
            self.share(entry, slot);
        }

        slot.record_dispatch(&job.rec);
        #[cfg(any(test, feature = "test-util"))]
        if matches!(job.rec.kind, DispatchKind::Http(_)) {
            entry.meters.record_dispatch();
        }
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);

        let dispatch = slot
            .ctx
            .start_dispatch(crate::now_ms(), job.rec.dispatch_id);
        let observer: Arc<dyn crate::reactor_accounting::Observer> = job.completion.clone();
        dispatch.reactor_attribution.set(Some(&observer));
        dispatch.set_cancel_future(job.rec.cancel_future);
        if let isoplane_abi::dispatch::DispatchKind::Http(request) = &job.rec.kind {
            *dispatch.request_body.lock() = Some(job.upload.clone().unwrap_or_else(|| {
                crate::bindings::request_body::RequestBodyScope::new(None, !request.body.is_empty())
            }));
        }

        *dispatch.transport.lock() = job.stream.clone();
        if let Some(stream) = &job.stream {
            let cell = Arc::downgrade(self);
            let owner = Arc::downgrade(slot);
            let cancelled = Arc::downgrade(&dispatch);
            let response = Arc::downgrade(stream);
            let did = job.rec.dispatch_id;
            stream.on_end_requested(Box::new(move || {
                if let (Some(cell), Some(slot), Some(response)) =
                    (cell.upgrade(), owner.upgrade(), response.upgrade())
                {
                    if response.is_cancelled() || response.failure().is_some() {
                        if let Some(dispatch) = cancelled.upgrade() {
                            dispatch.request_cancellation();
                        }
                        cell.arm_grace(&slot, did);
                    }
                    cell.queue_wake(slot);
                }
            }));
        }
        drop(dispatch);

        if self.end_stream_at_boundary(slot, entry, did, None) {
            return;
        }

        let outcome = {
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(_) => {
                    job.completion.select(TerminalReason::EntryRefused, 409);
                    let _completion = job.completion.settlement();
                    self.respond(
                        slot,
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
            job.completion.begin_slice();
            let out = tracing::info_span!("guest_slice")
                .in_scope(|| guard.instance().dispatch(&job.rec, budget));
            job.completion.add_slice(&out);
            entry.meters.record_occupancy(&out);
            if ends_the_instance(&out, slot) {
                guard.destroy();
            } else {
                self.retire_past_memory_limit(slot, entry, guard.instance().memory_len());
                drop(guard);
            }
            out
        };

        // A completion that landed while the dispatch was Hot waited on the
        // reentrancy rule; hand the instance to the wake lane for it.
        if !self.settle(slot, entry, did, outcome, &job.completion) {
            self.queue_wake(slot.clone());
        }
    }

    /// Whether a joined instance can begin another dispatch: it is resident,
    /// or restored from hibernation now.
    fn ready_to_join(self: &Arc<Self>, slot: &Arc<Slot>, entry: &Arc<ModuleEntry>) -> bool {
        match slot.state() {
            LifecycleTier::Discarded => false,
            LifecycleTier::Hibernated => self.restore_for_wake(slot, entry),
            LifecycleTier::Hot | LifecycleTier::Parked => true,
        }
    }

    /// The metrics for how a dispatch got its instance.
    fn note_acquired(&self, src: AcquireSource, took: Duration) {
        match src {
            AcquireSource::WarmParked => {}
            AcquireSource::HibernationWake => {
                self.metrics
                    .hibernation_wake_us
                    .record(took.as_micros() as u64);
            }
            AcquireSource::Activated => {
                self.metrics.activation_us.record(took.as_micros() as u64);
                #[cfg(any(test, feature = "test-util"))]
                self.metrics
                    .served_activated
                    .fetch_add(1, Ordering::Relaxed);
            }
            AcquireSource::Shared => {
                #[cfg(any(test, feature = "test-util"))]
                self.metrics.served_shared.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(super) fn queue_wake(&self, slot: Arc<Slot>) {
        self.queue.push_wake(WakeItem { slot });
    }

    /// Runs one guest slice, then returns remaining work to the queue.
    pub(super) fn run_wake(self: &Arc<Self>, slot: Arc<Slot>) {
        let _execution = slot.lock_execution();
        if self
            .first_active(slot.id, |active| active.completion.entry.clone())
            .is_some_and(|entry| self.kill_after_grace(&slot, &entry))
        {
            return;
        }
        let request_ready = self.has_starts(slot.id) || self.has_runnable(&slot);
        let background_first = !request_ready || !slot.ctx.background.last_was_background();
        if !(background_first && self.run_background(&slot)) {
            slot.ctx.background.ran_background(false);
            if !self.begin_joined(&slot) {
                self.run_woken(&slot);
            }
        }
        self.schedule_remaining(&slot);
    }

    /// Begins one dispatch that joined while another slice held the instance.
    fn begin_joined(self: &Arc<Self>, slot: &Arc<Slot>) -> bool {
        let Some((queued, job)) = self
            .starts
            .with_mut_or_remove(&slot.id, VecDeque::pop_front, VecDeque::is_empty)
            .flatten()
        else {
            return false;
        };
        let owner = job.completion.clone();
        self.waited_for_thread(&owner, queued.elapsed());
        IN_FLIGHT.replace(Some(Arc::downgrade(&owner)));
        let entry = owner.entry.clone();
        owner.in_scope(|| self.begin_dispatch(slot, &entry, job, AcquireSource::Shared));
        true
    }

    pub(super) fn has_starts(&self, instance: InstanceId) -> bool {
        self.starts.contains_key(&instance)
    }

    /// Runs the instance's ready slices; true when none remain.
    fn run_woken(self: &Arc<Self>, slot: &Arc<Slot>) -> bool {
        let slot = slot.clone();
        let Some(entry) = self.first_active(slot.id, |dispatch| dispatch.completion.entry.clone())
        else {
            return true;
        };
        if self.kill_after_grace(&slot, &entry) {
            return true;
        }
        for dispatch in self.active_dispatch_ids(slot.id) {
            if self.end_stream_at_boundary(&slot, &entry, dispatch, None)
                && !self.active.contains_key(&slot.id)
            {
                return true;
            }
        }
        if slot.state() == LifecycleTier::Hibernated && !self.restore_for_wake(&slot, &entry) {
            return true;
        }

        loop {
            // The instance is entered before the result is taken out, so a
            // result is never pulled with nowhere to deliver it. A busy
            // instance drains the result itself when its call returns.
            let guard = match slot.enter() {
                Ok(g) => g,
                Err(EnterError::Busy) => return true,
                Err(EnterError::NotResident(_)) => {
                    self.fail_instance(
                        &slot,
                        TerminalReason::EntryRefused,
                        409,
                        "instance_unavailable",
                    );
                    return true;
                }
            };
            let Some(ready) = self.rt.futures.take_ready(slot.id) else {
                // The first wake's loop drained every completion it can run.
                return true;
            };
            let Some(owner) = self.with_active(slot.id, ready.dispatch, |dispatch| {
                dispatch.completion.clone()
            }) else {
                tracing::debug!(instance = %slot.id, dispatch = ready.dispatch,
                    "dropped a result for a dispatch that has ended");
                continue;
            };
            IN_FLIGHT.replace(Some(Arc::downgrade(&owner)));
            return owner.in_scope(|| self.run_woken_slice(&slot, &entry, guard, ready, &owner));
        }
    }

    /// Restores a hibernated instance before its next slice; false if it
    /// could not be restored, in which case every dispatch on it has failed.
    pub(super) fn restore_for_wake(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
    ) -> bool {
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
            self.fail_instance(slot, TerminalReason::RestoreFailed, 500, "restore_failed");
            return false;
        }
        self.metrics
            .hibernation_wake_us
            .record(started.elapsed().as_micros() as u64);
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
        if let Some(waited) = ready.waited {
            self.waited_for_thread(owner, waited);
        }
        slot.ctx.begin_slice(crate::now_ms(), ready.dispatch);

        owner.begin_slice();
        let outcome = tracing::info_span!("guest_slice").in_scope(|| {
            guard
                .instance()
                .wake(ready.dispatch, ready.future, &ready.result, budget)
        });
        owner.add_slice(&outcome);
        entry.meters.record_occupancy(&outcome);
        if ends_the_instance(&outcome, slot) {
            guard.destroy();
        } else {
            self.retire_past_memory_limit(slot, entry, guard.instance().memory_len());
            drop(guard);
        }

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
        self.arm_grace(slot, did);
        // A latched trap becomes a trap here, before the guest proceeds as
        // though the import had succeeded. An exit is left alone: its
        // latched code is almost always a poisoned import the guest ignored
        // on its way out, and its slot is consistent.
        let latched = slot.ctx.take_trap();
        let mut latched_msg = None;
        let completion = match latched {
            Some(code)
                if matches!(
                    out.completion,
                    Completion::Done
                        | Completion::Cancelled
                        | Completion::Failed
                        | Completion::Parked
                ) =>
            {
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
        let completion = if completion == Completion::Cancelled
            && !slot
                .ctx
                .dispatch(did)
                .is_some_and(|dispatch| dispatch.cancellation_requested())
        {
            Completion::Failed
        } else if completion == Completion::Parked && closed_response.is_some() {
            Completion::Done
        } else {
            completion
        };

        let (module, route) = self
            .with_active(slot.id, did, |req| (req.module.clone(), req.route.clone()))
            .unwrap_or_else(|| (entry.activator.module().clone(), String::new()));
        let breaker_key = BreakerKey { module, route };
        if self.end_stream_at_boundary(slot, entry, did, Some(out.completion)) {
            return true;
        }

        if out.completion == Completion::Parked
            && !matches!(completion, Completion::Trapped | Completion::BudgetKill)
            && slot
                .ctx
                .dispatch(did)
                .is_some_and(|dispatch| dispatch.cancellation_requested())
        {
            return self.park_after_slice(slot, entry);
        }

        match completion {
            Completion::Parked => self.park_after_slice(slot, entry),
            Completion::Cancelled | Completion::Failed => {
                let (status, reason) = if completion == Completion::Cancelled {
                    (499, "request_cancelled")
                } else {
                    (500, "request_failed")
                };
                let result = owner.result(
                    slot.id,
                    status,
                    vec![],
                    vec![],
                    completion,
                    Some(reason.into()),
                );
                self.respond(slot, did, result);
                self.finish_with_instance_work_handover(slot, entry, did, warm())
            }
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
                    DispatchEnd::RetainedForStreaming => self.park_after_slice(slot, entry),
                    DispatchEnd::Disposed(d) => {
                        self.breaker.record_success(&breaker_key);
                        self.finish_with_instance_work_handover(slot, entry, did, d)
                    }
                    DispatchEnd::Parked => self.park_after_slice(slot, entry),
                }
            }
            // The dispatch still fails with `budget_exceeded`, but nothing was
            // killed: the instance stays warm, and the breaker records neither
            // a kill nor a success, since an exit is the budget working as
            // designed.
            Completion::BudgetExit => {
                let overage = out.overage_ns;
                #[cfg(any(test, feature = "test-util"))]
                {
                    entry
                        .meters
                        .record_grace_end(isoplane_engine::GraceEnd::Exited, overage);
                    self.metrics.budget_exits.fetch_add(1, Ordering::Relaxed);
                    self.metrics
                        .overage_ns
                        .fetch_add(overage, Ordering::Relaxed);
                }
                tracing::info!(
                    tenant = %entry.tenant, instance = %slot.id,
                    overage_us = overage / 1_000,
                    "budget exit: dispatch returned inside its grace window"
                );
                let msg = format!(
                    "budget_exceeded: occupancy budget elapsed; the dispatch returned inside its \
                     grace window and used {:.3} ms of overage",
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
                    DispatchEnd::Disposed(d) => {
                        self.finish_with_instance_work_handover(slot, entry, did, d)
                    }
                    DispatchEnd::Parked | DispatchEnd::RetainedForStreaming => {
                        self.park_after_slice(slot, entry)
                    }
                }
            }
            Completion::Trapped | Completion::BudgetKill => {
                let killed = completion == Completion::BudgetKill;
                if killed {
                    #[cfg(any(test, feature = "test-util"))]
                    {
                        entry
                            .meters
                            .record_grace_end(isoplane_engine::GraceEnd::Killed, out.overage_ns);
                        self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
                        self.metrics
                            .overage_ns
                            .fetch_add(out.overage_ns, Ordering::Relaxed);
                    }
                    self.breaker.record_kill(&breaker_key, crate::now_ms());
                } else {
                    // A kill is not also a trap.
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

    /// Retires an instance whose linear memory has passed its memory limit.
    /// Memory never shrinks, so the instance would stay past it: it takes no
    /// new dispatches, finishes those it has, and is then discarded, so the
    /// next dispatch gets a fresh instance from the template.
    pub(super) fn retire_past_memory_limit(
        &self,
        slot: &Arc<Slot>,
        entry: &ModuleEntry,
        memory_bytes: usize,
    ) {
        let limit = slot.ctx.limits.instance_memory_max;
        if memory_bytes as u64 <= limit || !slot.retire() {
            return;
        }
        #[cfg(any(test, feature = "test-util"))]
        self.metrics
            .memory_limit_retirements
            .fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            tenant = %entry.tenant, instance = %slot.id,
            memory_mib = memory_bytes >> 20, limit_mib = limit >> 20,
            "instance passed its memory limit: retiring it once its dispatches finish"
        );
    }

    /// Records how long a slice of `owner` waited for its instance's thread
    /// after it was ready to run.
    pub(super) fn waited_for_thread(&self, owner: &DispatchCompletion, waited: Duration) {
        owner.add_thread_wait(waited);
    }

    /// Ends the dispatch and, when retaining its instance, hands remaining
    /// futures to the separately budgeted instance work.
    fn finish_with_instance_work_handover(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        did: u64,
        d: crate::disposition::Disposition,
    ) -> bool {
        if d.is_warm() {
            self.end_request_body(slot, did);
            self.rt.futures.hand_to_instance(slot.id, did);
        }
        self.finish_and_release(slot, entry, did, d)
    }

    /// The response is complete, so the request is over: a read of its body
    /// still pending can never be answered with data, and is failed so the
    /// code waiting on it can finish.
    fn end_request_body(&self, slot: &Slot, did: u64) {
        for read in self
            .rt
            .futures
            .pending_of_dispatch(slot.id, did, "req_body_read")
        {
            self.rt.settle_future(
                read,
                isoplane_abi::result::ResultRecord::Err(
                    isoplane_abi::result::FutureError::Cancelled,
                ),
            );
        }
    }

    /// The instance parks between slices. Returns whether the wake loop is
    /// done with it.
    fn park_after_slice(self: &Arc<Self>, slot: &Arc<Slot>, entry: &Arc<ModuleEntry>) -> bool {
        if let Err(refused) = slot.park() {
            self.end_unparkable_instance(slot, entry, refused);
            return true;
        }
        // A completion that landed while Hot is drained now.
        !self.has_runnable(slot)
    }

    /// Whether a request has a result it can run now.
    pub(super) fn has_runnable(&self, slot: &Slot) -> bool {
        self.rt.futures.has_runnable(slot.id)
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

    /// Answers a dispatch whose response the front end has ended: its client
    /// went away, its transport failed, or it sent a head that has no body.
    /// A bodyless response settles successfully without cancelling guest work.
    /// Transport failure or requester loss asks the guest to stop within grace.
    /// Returns true when the dispatch has ended.
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
        let ended_by_client = stream.is_cancelled() || failure.is_some();
        let head = if ended_by_client {
            None
        } else {
            let Some(head) = stream.no_body_head() else {
                return false;
            };
            // A trap in the same slice is the dispatch's outcome, not the head.
            if completion.is_some_and(|completion| {
                !matches!(completion, Completion::Done | Completion::Parked)
            }) {
                return false;
            }
            Some(head)
        };
        let unanswered = self
            .with_active(slot.id, did, |active| active.responder.is_some())
            .unwrap_or(false);
        if unanswered {
            let result = match head {
                Some((status, headers)) => {
                    owner.result(slot.id, status, headers, vec![], Completion::Done, None)
                }
                None => {
                    let status = if failure.is_some() { 502 } else { 499 };
                    let terminal = if failure.is_some() {
                        TerminalReason::TransportFailure
                    } else {
                        TerminalReason::RequestCancelled
                    };
                    owner.select(terminal, status);
                    let reason = failure.unwrap_or_else(|| "request_cancelled".into());
                    owner.result(
                        slot.id,
                        status,
                        vec![],
                        vec![],
                        Completion::Trapped,
                        Some(reason),
                    )
                }
            };
            self.respond(slot, did, result);
            // The response is final: the front end sends it now, whatever the
            // guest does next.
            drop(stream.clone().settlement());
        }
        if !ended_by_client {
            return false;
        }
        dispatch.request_cancellation();
        // The guest may be parked with nothing else to wake it, and no slice
        // is running, so the request reaches it now rather than at the end of
        // a slice.
        if let Some(cancel) = dispatch.take_undelivered_cancellation() {
            self.rt.settle_future(
                cancel,
                isoplane_abi::result::ResultRecord::Err(
                    isoplane_abi::result::FutureError::Cancelled,
                ),
            );
        }
        drop(dispatch);
        match completion {
            Some(Completion::Done | Completion::Cancelled | Completion::Failed) => {
                self.finish_with_instance_work_handover(slot, entry, did, warm())
            }
            Some(Completion::Parked) | None => {
                self.arm_grace(slot, did);
                false
            }
            Some(Completion::Trapped | Completion::BudgetKill | Completion::BudgetExit) => false,
        }
    }

    /// Starts the grace of a dispatch whose cancellation has been requested,
    /// once: when it runs out, a wake finds the dispatch and kills it if it
    /// is still in progress.
    pub(super) fn arm_grace(self: &Arc<Self>, slot: &Arc<Slot>, did: u64) {
        if !slot
            .ctx
            .dispatch(did)
            .is_some_and(|dispatch| dispatch.cancellation_requested())
        {
            return;
        }
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(self.cfg.cancellation_grace_ms);
        if !self.start_grace(slot.id, did, deadline) {
            return;
        }
        let cell = Arc::downgrade(self);
        let watched = Arc::downgrade(slot);
        self.rt.reactor.spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let (Some(cell), Some(slot)) = (cell.upgrade(), watched.upgrade()) else {
                return;
            };
            if cell.with_active(slot.id, did, |_| ()).is_some() {
                cell.queue_wake(slot);
            }
        });
    }

    /// Kills the dispatches whose grace has run out, evicting the instance
    /// and failing every other dispatch on it. Returns true if it did.
    fn kill_after_grace(self: &Arc<Self>, slot: &Arc<Slot>, entry: &Arc<ModuleEntry>) -> bool {
        let now = tokio::time::Instant::now();
        let (expired, others): (Vec<_>, Vec<_>) = self
            .active
            .with(&slot.id, |active| {
                active
                    .iter()
                    .map(|dispatch| {
                        let expired = dispatch.grace_deadline.is_some_and(|at| at <= now);
                        (dispatch.dispatch_id, expired)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_iter()
            .partition(|(_, expired)| *expired);
        let Some(((last, _), expired)) = expired.split_last() else {
            return false;
        };
        tracing::warn!(
            tenant = %entry.tenant, instance = %slot.id,
            dispatches = ?expired.iter().map(|(id, _)| *id).chain([*last]).collect::<Vec<_>>(),
            grace_ms = self.cfg.cancellation_grace_ms,
            "dispatch still in progress after the grace for its cancellation: killing its instance"
        );
        for (dispatch, _) in others {
            self.fail_dispatch(
                slot,
                dispatch,
                TerminalReason::InstanceDiscarded,
                503,
                "instance_discarded",
            );
            self.end_dispatch_books(slot, dispatch);
        }
        for &(dispatch, _) in expired {
            self.fail_grace_expired(slot, dispatch);
            self.end_dispatch_books(slot, dispatch);
        }
        self.fail_grace_expired(slot, *last);
        slot.discard();
        self.finish_and_release(slot, entry, *last, evict());
        true
    }

    fn fail_grace_expired(&self, slot: &Arc<Slot>, dispatch: u64) {
        if let Some(key) = self.with_active(slot.id, dispatch, |active| BreakerKey {
            module: active.module.clone(),
            route: active.route.clone(),
        }) {
            self.breaker.record_kill(&key, crate::now_ms());
        }
        #[cfg(any(test, feature = "test-util"))]
        self.metrics
            .cancellation_grace_kills
            .fetch_add(1, Ordering::Relaxed);
        self.fail_dispatch(
            slot,
            dispatch,
            TerminalReason::CancellationGraceExpired,
            503,
            "cancellation_grace_expired",
        );
    }

    /// Fails every dispatch in progress on an instance that cannot continue,
    /// then evicts it. An instance with no dispatch in progress and no place
    /// reserved is idle: its pool holds it, and whoever took it from there
    /// ends it.
    pub(super) fn fail_instance(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        reason: TerminalReason,
        status: u16,
        code: &'static str,
    ) {
        if self.active.contains_key(&slot.id) {
            self.end_instance(slot, reason, status, code);
        }
    }

    /// Ends an instance the cell holds outside its pool, once: its dispatches
    /// in progress fail with `reason`, the starts queued for it are refused,
    /// and it leaves the cell and its pool. A place reserved by a dispatch
    /// that has not queued its start yet stays on the books until that
    /// dispatch finds the instance gone and gives the place up.
    pub(super) fn end_instance(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        reason: TerminalReason,
        status: u16,
        code: &'static str,
    ) {
        for dispatch in self.active_dispatch_ids(slot.id) {
            self.fail_dispatch(slot, dispatch, reason, status, code);
            self.end_dispatch_books(slot, dispatch);
        }
        self.evict_instance(slot);
    }

    /// Takes an instance with no dispatch left in progress out of the cell
    /// and its pool, refusing the starts still queued for it.
    fn evict_instance(&self, slot: &Arc<Slot>) {
        self.refuse_queued_starts(slot.id);
        self.forget(slot);
        self.rt.futures.release_instance(slot.id);
        slot.discard();
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
        let mut abandoned_guest_work = false;
        if let Some(r) = &req {
            if r.completion.is_http {
                slot.ctx
                    .background
                    .request_finished(Duration::from_millis(self.cfg.background_idle_limit_ms));
            }
            if did != crate::futures::INSTANCE_WORK {
                r.completion.donate_to(&slot.ctx.background);
                abandoned_guest_work = self.rt.futures.end_dispatch(slot.id, did);
            }
            #[cfg(any(test, feature = "test-util"))]
            if let Some(total_wall_ns) = r.completion.total_wall_ns() {
                self.metrics.dispatch_us.record(total_wall_ns / 1_000);
            }
        }
        (req, remaining, abandoned_guest_work)
    }

    /// Ends a dispatch and, once none remain on the instance, releases the
    /// instance with disposition `d`. A disposition that loses the instance
    /// fails the dispatches still in progress on it. Returns true: the wake
    /// loop is done with the instance.
    pub(super) fn finish_and_release(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        did: u64,
        d: crate::disposition::Disposition,
    ) -> bool {
        let (_req, remaining, abandoned_guest_work) = self.end_dispatch_books(slot, did);
        let d = if abandoned_guest_work || (remaining == 0 && slot.is_retiring()) {
            evict()
        } else {
            d
        };
        let keeps_instance = d.is_warm();
        if remaining > 0 {
            if keeps_instance {
                return self.park_after_slice(slot, entry);
            }
            for dispatch in self.active_dispatch_ids(slot.id) {
                self.fail_dispatch(
                    slot,
                    dispatch,
                    TerminalReason::InstanceDiscarded,
                    503,
                    "instance_discarded",
                );
                self.end_dispatch_books(slot, dispatch);
            }
        }

        if !keeps_instance {
            self.evict_instance(slot);
            return true;
        }
        self.rt.futures.release_idle(slot.id);
        self.release_to_pool(entry, slot);
        if slot.state() == LifecycleTier::Discarded {
            self.forget(slot);
        } else {
            self.watch_idle_timers(slot, entry);
            self.schedule_remaining(slot);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::pool::cell_tests::{app, cell, http};
    use crate::fakes::{FakeBehaviour, Step};
    use isoplane_test_support::wait_for;

    type Answer = std::sync::mpsc::Receiver<DispatchResult>;

    fn job(
        cell: &Arc<Cell>,
        module: &ModuleId,
        did: u64,
        stream: Option<Arc<crate::response_transport::ResponseTransport>>,
    ) -> (DispatchJob, Answer, Arc<DispatchCompletion>) {
        let rec = http(did);
        let owner = cell.completion_owner(module, &rec).expect("owner");
        let (sender, response) = std::sync::mpsc::sync_channel(1);
        let job = DispatchJob {
            module: module.clone(),
            target: Target::Handler { route: "/".into() }.into(),
            rec,
            responder: sender.into(),
            stream,
            upload: None,
            completion: owner.clone(),
        };
        (job, response, owner)
    }

    /// Runs a dispatch on its own thread up to the slice `entered` reports,
    /// which stays open until the test lets go of the step's `release`.
    fn hold_the_last_slice(
        cell: &Arc<Cell>,
        module: &ModuleId,
        did: u64,
        entered: &std::sync::mpsc::Receiver<()>,
    ) -> (std::thread::JoinHandle<()>, Arc<Slot>, Answer) {
        let (job, response, owner) = job(cell, module, did, None);
        let running = {
            let cell = cell.clone();
            let subscriber = tracing::dispatcher::get_default(Clone::clone);
            std::thread::spawn(move || {
                tracing::dispatcher::with_default(&subscriber, || cell.run_dispatch(job))
            })
        };
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("the slice must start");
        let slot = cell
            .slot(owner.instance().expect("instance acquired"))
            .expect("resident");
        (running, slot, response)
    }

    /// Holds a dispatch's last slice open while `discard` ends its instance
    /// from outside, then lets the slice return.
    fn discard_during_the_last_slice(
        cell: &Arc<Cell>,
        module: &ModuleId,
        did: u64,
        entered: &std::sync::mpsc::Receiver<()>,
        release: std::sync::mpsc::Sender<()>,
    ) -> (Arc<Slot>, DispatchResult) {
        let (running, slot, response) = hold_the_last_slice(cell, module, did, entered);
        slot.discard();
        drop(release);
        running.join().expect("the worker must not panic");
        let result = response.try_recv().expect("the dispatch is answered");
        (slot, result)
    }

    /// Asserts that a dispatch which never began on its instance was told the
    /// instance is gone.
    fn assert_refused_for_a_discarded_instance(response: &Answer, did: u64) {
        let refused = response
            .try_recv()
            .unwrap_or_else(|_| panic!("dispatch {did} must be answered"));
        assert_eq!(
            (refused.status, refused.body.as_slice()),
            (503, b"instance_discarded".as_slice()),
            "dispatch {did} is refused for the instance it could not start on"
        );
    }

    /// Runs the one wake queued for `slot`.
    fn run_the_queued_wake(cell: &Arc<Cell>, slot: &Arc<Slot>) {
        assert_eq!(cell.queue.depth(), 1, "the queued starts share one wake");
        let Job::Wake(wake) = cell.queue.pop(0).expect("a wake is queued") else {
            panic!("wake expected")
        };
        assert_eq!(wake.slot.id, slot.id, "the wake is the instance's");
        cell.run_wake(wake.slot);
    }

    /// Asserts that nothing of an ended instance remains in the cell or its
    /// pool.
    fn assert_instance_fully_released(cell: &Arc<Cell>, module: &ModuleId, slot: &Arc<Slot>) {
        assert!(
            !cell.active.contains_key(&slot.id),
            "no dispatch or reserved place stays on the lost instance"
        );
        assert!(!cell.has_starts(slot.id), "no start stays queued for it");
        assert!(
            cell.slot(slot.id).is_none(),
            "the discarded instance leaves the cell"
        );
        assert_eq!(cell.idle_instances(module), Some(0), "and its pool");
        assert_eq!(cell.futures().live_futures(), 0);
    }

    #[test]
    fn joins_queued_behind_the_last_slice_of_a_discarded_instance_are_all_refused() {
        use crate::cell::completion::test_events::Events;
        use crate::cell::pool::cell_tests::manifest;
        use crate::fakes::FakeActivator;
        let events = Events::default();
        let _trace = events.install();
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..crate::fakes::cell_config("discarded-with-joins")
        });
        let (step, entered, release) = hold();
        let module = ModuleId::new("discarded-with-joins", "v1");
        // One instance at most, so the second join queues behind the first
        // rather than activating an instance of its own.
        let mut manifest = manifest("discarded-with-joins");
        manifest.resource_limits.instances_per_application = Some(1);
        cell.register_module(
            "acme",
            Arc::new(FakeActivator::named(
                module.clone(),
                FakeBehaviour::program([step]),
            )),
            manifest,
        )
        .expect("register");

        let (running, slot, last) = hold_the_last_slice(&cell, &module, 1, &entered);
        let joins = [2, 3].map(|did| {
            let (job, response, _) = job(&cell, &module, did, None);
            cell.run_dispatch(job);
            (did, response)
        });
        assert_eq!(
            cell.starts.with(&slot.id, VecDeque::len),
            Some(2),
            "both joins wait on the instance for its running slice"
        );
        slot.discard();
        drop(release);
        running.join().expect("the worker must not panic");

        assert_eq!(
            last.try_recv().expect("the dispatch is answered").status,
            200,
            "the finished dispatch keeps its answer"
        );
        for (did, response) in &joins {
            assert_refused_for_a_discarded_instance(response, *did);
        }
        assert_eq!(
            cell.metrics().refused_transitions,
            1,
            "the refused park is counted once"
        );
        assert_instance_fully_released(&cell, &module, &slot);
        run_the_queued_wake(&cell, &slot);
        assert_instance_fully_released(&cell, &module, &slot);
        cell.shutdown();
        let mut outcomes: Vec<_> = events
            .records()
            .iter()
            .map(|record| record["outcome"].as_str().expect("outcome").to_owned())
            .collect();
        outcomes.sort();
        assert_eq!(
            outcomes,
            ["instance_discarded", "instance_discarded", "success"],
            "each dispatch completes exactly once"
        );
    }

    #[test]
    fn starts_queued_after_their_instance_ended_are_all_refused_by_its_next_wake() {
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..crate::fakes::cell_config("late-joins")
        });
        let (step, entered, release) = hold();
        let module = app(&cell, "late-joins", FakeBehaviour::program([step]));
        let entry = cell.modules.read()[&module].clone();

        let (running, slot, last) = hold_the_last_slice(&cell, &module, 1, &entered);
        let late = [2, 3].map(|did| {
            assert!(
                matches!(
                    cell.join_shared(&entry, true),
                    registry::Join::Joined(joined) if joined.id == slot.id
                ),
                "dispatch {did} reserves a place on the running instance"
            );
            let (job, response, _) = job(&cell, &module, did, None);
            (did, job, response)
        });
        slot.discard();
        drop(release);
        running.join().expect("the worker must not panic");
        assert_eq!(
            last.try_recv().expect("the dispatch is answered").status,
            200,
            "the finished dispatch keeps its answer"
        );
        assert!(
            cell.slot(slot.id).is_none(),
            "the instance does not wait for the places still reserved on it"
        );

        // Both starts find the instance busy, as they would behind the slice
        // that ended it, and so share one wake.
        let answers = {
            let _execution = slot.lock_execution();
            late.map(|(did, job, response)| {
                cell.start_shared(&slot, &entry, job);
                (did, response)
            })
        };
        run_the_queued_wake(&cell, &slot);
        for (did, response) in &answers {
            assert_refused_for_a_discarded_instance(response, *did);
        }
        assert_instance_fully_released(&cell, &module, &slot);
        cell.shutdown();
    }

    fn hold() -> (
        Step,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered, started) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let step = Step::Hold {
            entered,
            release: Arc::new(Mutex::new(held)),
        };
        (step, started, release)
    }

    #[test]
    fn an_instance_discarded_during_its_last_slice_is_not_pooled() {
        use crate::cell::completion::test_events::Events;
        let events = Events::default();
        let _trace = events.install();
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..crate::fakes::cell_config("discarded-mid-slice")
        });
        let (step, entered, release) = hold();
        let module = app(&cell, "discarded-mid-slice", FakeBehaviour::program([step]));
        let (slot, result) = discard_during_the_last_slice(&cell, &module, 1, &entered, release);

        assert_eq!(result.status, 200, "the finished dispatch keeps its answer");
        assert_eq!(
            slot.state(),
            LifecycleTier::Discarded,
            "the returning slice must not revive the instance"
        );
        assert_eq!(
            cell.metrics().refused_transitions,
            1,
            "the refused park is counted"
        );
        assert_eq!(
            cell.idle_instances(&module),
            Some(0),
            "a discarded instance is not offered to the next dispatch"
        );
        assert!(
            cell.slot(slot.id).is_none(),
            "the discarded instance leaves the cell"
        );
        assert_eq!(cell.futures().live_futures(), 0);
        cell.shutdown();
        let records = events.records();
        assert_eq!(records.len(), 1, "one completion for the one dispatch");
        assert_eq!(records[0]["outcome"], "success");
    }

    #[test]
    fn a_refused_park_fails_the_dispatches_still_on_the_instance() {
        use crate::cell::completion::test_events::Events;
        let events = Events::default();
        let _trace = events.install();
        let cell = cell(CellConfig {
            exec_threads: 0,
            ..crate::fakes::cell_config("refused-park")
        });
        let (step, entered, release) = hold();
        let module = app(
            &cell,
            "refused-park",
            FakeBehaviour::program([
                Step::Park {
                    imports: vec![("clock_deadline_in", 600_000)],
                },
                step,
            ]),
        );
        let (waiting, waiting_response, waiting_owner) = job(&cell, &module, 1, None);
        cell.run_dispatch(waiting);
        let (slot, result) = discard_during_the_last_slice(&cell, &module, 2, &entered, release);
        assert_eq!(
            waiting_owner.instance(),
            Some(slot.id),
            "both dispatches must share the instance"
        );

        assert_eq!(result.status, 200, "the finished dispatch keeps its answer");
        let failed = waiting_response
            .try_recv()
            .expect("the waiting dispatch is answered");
        assert_eq!(
            (failed.status, failed.trap.as_deref()),
            (503, Some("instance_discarded")),
            "a dispatch waiting on the lost instance fails with it"
        );
        assert_eq!(
            cell.metrics().refused_transitions,
            1,
            "the refused park is counted once"
        );
        assert!(
            !cell.active.contains_key(&slot.id),
            "no dispatch stays on the lost instance"
        );
        assert!(
            cell.slot(slot.id).is_none(),
            "the discarded instance leaves the cell"
        );
        assert_eq!(cell.futures().live_futures(), 0);
        cell.shutdown();
        let mut outcomes: Vec<_> = events
            .records()
            .iter()
            .map(|record| record["outcome"].as_str().expect("outcome").to_owned())
            .collect();
        outcomes.sort();
        assert_eq!(
            outcomes,
            ["instance_discarded", "success"],
            "each dispatch completes exactly once"
        );
    }

    #[test]
    fn queued_instances_are_joined_only_when_activation_cannot_serve() {
        use crate::cell::pool::cell_tests::manifest;
        use crate::fakes::FakeActivator;
        for limit in [1, 2] {
            let cell = cell(CellConfig {
                exec_threads: 0,
                ..crate::fakes::cell_config("join-fallback")
            });
            let module = ModuleId::new("join-fallback", "v1");
            let mut manifest = manifest("join-fallback");
            manifest.resource_limits.instances_per_application = Some(limit);
            cell.register_module(
                "tenant",
                Arc::new(FakeActivator::named(
                    module.clone(),
                    FakeBehaviour::program([
                        Step::Park {
                            imports: vec![("clock_deadline_in", 600_000)],
                        },
                        Step::Done {
                            status: 200,
                            body: vec![],
                        },
                    ]),
                )),
                manifest,
            )
            .expect("register");
            let make_job = |did| {
                let rec = http(did);
                let completion = cell.completion_owner(&module, &rec).expect("owner");
                let (sender, response) = std::sync::mpsc::sync_channel(1);
                (
                    DispatchJob {
                        module: module.clone(),
                        target: Target::Handler { route: "/".into() }.into(),
                        rec,
                        responder: sender.into(),
                        stream: None,
                        upload: None,
                        completion: completion.clone(),
                    },
                    response,
                    completion,
                )
            };
            let (first, _first_response, first_owner) = make_job(1);
            cell.run_dispatch(first);
            let slot = cell
                .slot(first_owner.instance().expect("first instance"))
                .expect("resident");
            let future = cell
                .rt
                .futures
                .pending_of_dispatch(slot.id, 1, "clock_deadline_in")[0];
            cell.complete_future(future, isoplane_abi::result::ResultRecord::Ok(vec![]));
            assert!(cell.has_runnable(&slot));
            let (second, second_response, second_owner) = make_job(2);
            cell.run_dispatch(second);
            if limit == 1 {
                assert_eq!(
                    second_response.try_recv().expect("joined response").status,
                    200
                );
                assert_eq!(second_owner.instance(), Some(slot.id));
            } else {
                assert_ne!(second_owner.instance(), Some(slot.id));
                assert!(second_response.try_recv().is_err(), "fresh instance parked");
            }
            cell.shutdown();
        }
    }

    #[test]
    fn a_queued_join_rechecks_memory_before_starting() {
        let storage = tempfile::tempdir().expect("snapshot directory");
        let cell = Cell::build(
            CellConfig {
                exec_threads: 1,
                ..crate::fakes::cell_config("join-recheck")
            },
            crate::cell::pool::cell_tests::services(),
            Arc::new(
                isoplane_hibernate::snapshot::LocalSnapshotStorage::new(storage.path())
                    .expect("snapshot storage"),
            ),
        )
        .expect("cell without workers");
        let wasm = wat::parse_str(
            r#"(module
            (memory (export "memory") 1)
            (func (export "isoplane_alloc") (param i32 i32) (result i32) i32.const 4096)
            (func (export "isoplane_handle") (param i32 i32) (result i64) i64.const 1)
            (func (export "isoplane_wake") (param i64 i32 i32) (result i64)
                (drop (memory.grow (i32.const 1))) i64.const 1))"#,
        )
        .expect("memory growth guest");
        let engine = Arc::new(
            isoplane_engine::IsoplaneEngine::new(isoplane_engine::EngineConfig {
                max_instances: 2,
                max_memory_bytes: 4 * isoplane_abi::WASM_PAGE_SIZE as u64,
                ..Default::default()
            })
            .expect("engine"),
        );
        let module = ModuleId::new("join-recheck", "v1");
        let mut manifest = crate::cell::pool::cell_tests::manifest("join-recheck");
        manifest.guest_memory_bytes = Some(
            isoplane_abi::config::GuestMemoryBytes::new(2 * isoplane_abi::WASM_PAGE_SIZE as u64)
                .expect("two pages"),
        );
        cell.register_module(
            "tenant",
            Arc::new(
                engine
                    .load_template(&wasm)
                    .map(|template| crate::EngineActivator::new(engine, template, module.clone()))
                    .expect("template"),
            ),
            manifest,
        )
        .expect("register");
        let make_job = |did| {
            let rec = http(did);
            let owner = cell.completion_owner(&module, &rec).expect("completion");
            let (sender, _) = std::sync::mpsc::sync_channel(1);
            DispatchJob {
                module: module.clone(),
                target: Target::Handler { route: "/".into() }.into(),
                rec,
                responder: sender.into(),
                stream: None,
                upload: None,
                completion: owner,
            }
        };
        cell.run_dispatch(make_job(1));
        let slot = cell.slot(cell.resident_ids()[0]).expect("first instance");
        let entry = cell.modules.read()[&module].clone();
        let execution = slot.lock_execution();
        assert!(slot.has_memory_for_join());
        cell.run_dispatch(make_job(2));
        assert!(cell.has_starts(slot.id), "join waits for the running slice");
        {
            let mut guard = slot.enter().expect("memory growth slice");
            let result = guard.instance().wake(
                1,
                0,
                &isoplane_abi::result::ResultRecord::Ok(vec![]),
                OccupancyBudget::default(),
            );
            assert_eq!(result.completion, Completion::Parked);
        }
        assert!(!slot.has_memory_for_join());
        drop(execution);
        cell.run_wake(slot.clone());
        assert_eq!(
            cell.active_dispatch_ids(slot.id),
            vec![1],
            "the join did not start"
        );
        cell.queue.close();
        let mut rerouted = cell.queue.drain_stopped();
        assert_eq!(
            rerouted.len(),
            1,
            "the original job returns to admission once"
        );
        let job = rerouted.pop().expect("one rerouted job");
        let owner = job.completion.clone();
        cell.run_dispatch(job);
        assert_ne!(
            owner.instance(),
            Some(slot.id),
            "another instance handles the request"
        );
        assert_eq!(
            cell.metrics().served_shared,
            0,
            "a rerouted reservation is not served"
        );
        cell.finish_and_release(&slot, &entry, 1, warm());
        assert_eq!(
            cell.idle_instances(&module),
            Some(1),
            "the join reservation was released"
        );
        cell.shutdown();
    }

    #[test]
    fn a_wake_visit_runs_one_slice_and_requeues_remaining_completions() {
        let storage = tempfile::tempdir().expect("snapshot directory");
        let cell = Cell::build(
            CellConfig {
                exec_threads: 1,
                ..crate::fakes::cell_config("bounded-wake")
            },
            crate::cell::pool::cell_tests::services(),
            Arc::new(
                isoplane_hibernate::snapshot::LocalSnapshotStorage::new(storage.path())
                    .expect("snapshot storage"),
            ),
        )
        .expect("cell without worker threads");
        let module = app(
            &cell,
            "bounded-wake",
            FakeBehaviour::program([
                Step::Park {
                    imports: vec![("clock_deadline_in", 60_000), ("clock_deadline_in", 60_000)],
                },
                Step::Park { imports: vec![] },
                Step::Done {
                    status: 200,
                    body: b"done".to_vec(),
                },
            ]),
        );
        let rec = http(1);
        let owner = cell
            .completion_owner(&module, &rec)
            .expect("dispatch owner");
        let (sender, response) = std::sync::mpsc::sync_channel(1);
        cell.run_dispatch(DispatchJob {
            module,
            target: Target::Handler { route: "/".into() }.into(),
            rec,
            responder: sender.into(),
            stream: None,
            upload: None,
            completion: owner.clone(),
        });
        let slot = cell
            .slot(owner.instance().expect("instance acquired"))
            .expect("resident");
        let futures = cell
            .rt
            .futures
            .pending_of_dispatch(slot.id, 1, "clock_deadline_in");
        assert_eq!(futures.len(), 2);
        for future in futures {
            cell.rt
                .settle_future(future, isoplane_abi::result::ResultRecord::Ok(vec![]));
        }
        assert_eq!(cell.queue.depth(), 1);
        let Job::Wake(wake) = cell.queue.pop(0).expect("first wake") else {
            panic!("wake expected")
        };
        cell.run_wake(wake.slot);
        assert_eq!(
            owner.usage().slices,
            2,
            "initial dispatch plus one wake slice"
        );
        assert_eq!(
            response.try_recv().unwrap_err(),
            std::sync::mpsc::TryRecvError::Empty
        );
        assert_eq!(cell.queue.depth(), 1, "remaining completion is requeued");
        let Job::Wake(wake) = cell.queue.pop(0).expect("second wake") else {
            panic!("wake expected")
        };
        cell.run_wake(wake.slot);
        assert_eq!(owner.usage().slices, 3);
        assert_eq!(
            response.try_recv().expect("completed response").body,
            b"done"
        );
        cell.shutdown();
    }

    #[test]
    fn only_request_dispatches_refresh_instance_demand() {
        let cell = cell(crate::fakes::cell_config("demand-recency"));
        let module = app(&cell, "demand-recency", FakeBehaviour::done());
        let mut request = http(1);
        request.now_ms = 10;
        let result = cell
            .dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                request,
                Duration::from_secs(10),
            )
            .expect("request response");
        let slot = cell.slot(result.instance).expect("request instance");
        assert_eq!(slot.last_used_ms(), 10);
        assert!(wait_for(
            || cell.modules.read()[&module].pool.idle() == 1,
            Duration::from_secs(10),
        ));
        let mut timer = http(2);
        timer.kind = DispatchKind::Background;
        timer.now_ms = 100;
        let result = cell
            .dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                timer,
                Duration::from_secs(10),
            )
            .expect("timer result");
        assert_eq!(result.instance, slot.id);
        assert_eq!(slot.last_used_ms(), 10);
        cell.shutdown();
    }

    #[test]
    fn a_grace_kill_opens_only_the_responsible_routes_breaker() {
        let cell = cell(CellConfig {
            breaker: BreakerConfig {
                kill_threshold: 1,
                ..Default::default()
            },
            ..crate::fakes::cell_config("grace-breaker")
        });
        let module = app(
            &cell,
            "grace-breaker",
            FakeBehaviour::program([
                Step::Park { imports: vec![] },
                Step::Park { imports: vec![] },
            ]),
        );
        let waiting = cell
            .submit(
                &module,
                Target::Handler {
                    route: "/waiting".into(),
                },
                http(1),
            )
            .expect("first request");
        assert!(wait_for(
            || cell
                .resident_ids()
                .first()
                .and_then(|id| cell.slot(*id))
                .is_some_and(|slot| slot.parks() == 1),
            Duration::from_secs(10)
        ));
        let slot = cell.slot(cell.resident_ids()[0]).expect("parked instance");
        let ignoring = cell
            .submit(
                &module,
                Target::Handler {
                    route: "/ignoring".into(),
                },
                http(2),
            )
            .expect("second request");
        assert!(wait_for(|| slot.parks() == 2, Duration::from_secs(10)));
        {
            let _execution = slot.lock_execution();
            assert!(cell.start_grace(slot.id, 2, tokio::time::Instant::now()));
            slot.ctx.background.requested.store(true, Ordering::Release);
        }
        cell.run_wake(slot.clone());
        assert_eq!(
            ignoring
                .recv_timeout(Duration::from_secs(10))
                .expect("grace result")
                .trap
                .as_deref(),
            Some("cancellation_grace_expired")
        );
        assert_eq!(
            waiting
                .recv_timeout(Duration::from_secs(10))
                .expect("collateral result")
                .trap
                .as_deref(),
            Some("instance_discarded")
        );
        let refused = cell
            .submit(
                &module,
                Target::Handler {
                    route: "/ignoring".into(),
                },
                http(3),
            )
            .expect("breaker response");
        assert_eq!(
            refused
                .recv_timeout(Duration::from_secs(10))
                .expect("refused request")
                .trap
                .as_deref(),
            Some("breaker_open")
        );
        assert!(cell
            .breaker
            .check(
                &BreakerKey {
                    module,
                    route: "/waiting".into()
                },
                crate::now_ms()
            )
            .is_ok());
        cell.shutdown();
    }
}
