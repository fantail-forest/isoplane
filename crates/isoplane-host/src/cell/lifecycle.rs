//! One dispatch from admission to its final response: the request lifecycle across every slice, the outcome switch, and the books a tombstone keeps.

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
            Target::Handler { .. } => self.acquire_handler(&entry),
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

        let budget = CpuBudget {
            cpu_ms: job.rec.cpu_budget_ms,
            wall_ms_cap: self.cfg.wall_ms_cap,
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

        self.active.insert(
            slot.id,
            ActiveRequest {
                dispatch_id: job.rec.dispatch_id,
                lifecycle: entry.manifest.handlers.lifecycle,
                module: job.module.clone(),
                route: job.target.route(),
                responder: Some(job.responder),
                completion: job.completion.clone(),
            },
        );

        entry.meters.record_dispatch();
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);

        slot.ctx
            .begin_dispatch(crate::now_ms(), job.rec.dispatch_id);
        slot.ctx.set_cancel_future(job.rec.cancel_future);
        if let isoplane_abi::dispatch::DispatchKind::Http(request) = &job.rec.kind {
            *slot.ctx.request_body.lock() = Some(job.upload.clone().unwrap_or_else(|| {
                crate::bindings::request_body::RequestBodyScope::new(None, !request.body.is_empty())
            }));
        }

        slot.ctx.responses.lock().transport = job.stream.clone();
        if let Some(stream) = &job.stream {
            let cell = Arc::downgrade(self);
            let owner = Arc::downgrade(&slot);
            let generation = slot.ctx.dispatch_generation();
            let response = Arc::downgrade(stream);
            stream.on_end_requested(Box::new(move || {
                if let (Some(cell), Some(slot), Some(response)) =
                    (cell.upgrade(), owner.upgrade(), response.upgrade())
                {
                    if response.is_cancelled() || response.failure().is_some() {
                        slot.ctx.cancel_generation(generation);
                    }
                    cell.queue_wake(slot);
                }
            }));
        }

        if self.end_stream_at_boundary(&slot, &entry, None) {
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
                        DispatchResult {
                            status: 409,
                            body: b"instance busy".to_vec(),
                            ..DispatchResult::shed(did)
                        },
                    );
                    let _settlement = slot
                        .ctx
                        .responses
                        .lock()
                        .transport
                        .take()
                        .map(|stream| stream.settlement());
                    slot.ctx.end_http_dispatch();
                    self.rt.futures.end_dispatch(slot.id, did);
                    self.active.remove(&slot.id);
                    return;
                }
            };
            let hot = super::completion::HotInterval::new(&self.hot);
            job.completion.begin_slice();
            let out = tracing::info_span!("guest_slice")
                .in_scope(|| guard.instance().dispatch(&job.rec, budget));
            drop(hot);
            job.completion.add_slice(&out);
            entry.meters.record_cpu(&out);
            if ends_in_tombstone(&out, &slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            out
        };

        // A completion that landed while the dispatch was Hot waited on the
        // reentrancy rule; hand the instance to the wake lane for it.
        if !self.settle(&slot, &entry, outcome, &job.completion) {
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
    /// going back through a lane.
    pub(super) fn run_wake(self: &Arc<Self>, slot: Arc<Slot>) {
        let _execution = slot.lock_execution();
        let Some(owner) = self
            .active
            .with(&slot.id, |request| request.completion.clone())
        else {
            return;
        };
        IN_FLIGHT.replace(Some(Arc::downgrade(&owner)));
        owner.in_scope(|| self.run_owned_wake(&slot, &owner));
    }

    fn run_owned_wake(self: &Arc<Self>, slot: &Arc<Slot>, owner: &Arc<DispatchCompletion>) {
        let entry = owner.entry.clone();

        if self.end_stream_at_boundary(slot, &entry, None) {
            return;
        }

        if slot.state() == LifecycleTier::Hibernated {
            let started = Instant::now();
            if let Err(error) = self.wake(slot, &entry) {
                let message = format!(
                    "instance {} could not restore its pending dispatch: {error}",
                    slot.id
                );
                owner.select(TerminalReason::RestoreFailed, 500);
                let result = owner.result(
                    slot.id,
                    500,
                    vec![],
                    message.as_bytes().to_vec(),
                    Completion::Trapped,
                    Some(message),
                );
                self.respond(slot, result);
                self.finish_and_release(slot, &entry, evict());
                return;
            }
            self.metrics
                .hibernation_wake_us
                .record(started.elapsed().as_micros() as u64);
            self.metrics
                .hibernation_wakes
                .fetch_add(1, Ordering::Relaxed);
            entry.meters.record_hibernation_wake();
        }

        loop {
            // The instance is entered before the result is taken out, so a
            // result is never pulled with nowhere to deliver it. A busy
            // instance drains the result itself when its call returns.
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(EnterError::Busy) => {
                    self.count_enter_refusal(EnterError::Busy);
                    return;
                }
                Err(error @ (EnterError::Tombstoned | EnterError::NotResident(_))) => {
                    self.count_enter_refusal(error);
                    owner.select(TerminalReason::EntryRefused, 409);
                    self.respond(slot, owner.failure_result(409, "instance_unavailable"));
                    self.finish_and_release(
                        slot,
                        &entry,
                        evict(),
                    );
                    return;
                }
            };
            let Some((fid, res)) = self.rt.futures.take_ready(slot.id) else {
                // The first wake's loop drained every completion.
                return;
            };
            // The budget binds the dispatch, so a wake resumes with what
            // earlier slices burned; otherwise a guest could park and wake
            // indefinitely on a fresh budget each time.
            let budget = owner.next_budget();

            slot.ctx
                .begin_slice(crate::now_ms(), slot.ctx.dispatch_id());

            let t0 = Instant::now();
            let hot = super::completion::HotInterval::new(&self.hot);
            owner.begin_slice();
            let outcome = tracing::info_span!("guest_slice")
                .in_scope(|| guard.instance().wake(fid, &res, budget));
            drop(hot);
            owner.add_slice(&outcome);
            entry.meters.record_cpu(&outcome);
            if ends_in_tombstone(&outcome, slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            self.metrics.wake_us.record(t0.elapsed().as_micros() as u64);

            if self.settle(slot, &entry, outcome, owner) {
                return;
            }
        }
    }

    /// Returns true when the instance takes no more wakes in this loop.
    fn settle(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
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
            out.cpu_ns,
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

        let closed_response = slot.ctx.responses.lock().completed_transport_response();
        let completion = if completion == Completion::Parked && closed_response.is_some() {
            Completion::Done
        } else {
            completion
        };

        let (lifecycle, module, route) = self
            .active
            .with_mut(&slot.id, |req| {
                (req.lifecycle, req.module.clone(), req.route.clone())
            })
            .unwrap_or_else(|| {
                (
                    entry.manifest.handlers.lifecycle,
                    entry.activator.module().clone(),
                    String::new(),
                )
            });
        let breaker_key = BreakerKey { module, route };
        if self.end_stream_at_boundary(slot, entry, Some(completion)) {
            return true;
        }

        match completion {
            Completion::Parked => self.park_after_slice(slot, ),
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
                if !streaming_open || slot.ctx.responses.lock().transport.is_none() {
                    self.respond(slot, result);
                }

                match decide(lifecycle, completion, streaming_open) {
                    RequestEnd::RetainedForStreaming => {
                        slot.park(crate::now_ms());
                        !self.rt.futures.has_ready(slot.id)
                    }
                    RequestEnd::Disposed(d) => {
                        self.breaker.record_success(&breaker_key);
                        self.finish_and_release(slot, entry, d);
                        true
                    }
                    RequestEnd::Parked => self.park_after_slice(slot, ),
                }
            }
            // The request still fails with `budget_exceeded`, but the
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
                    "budget_exceeded: cpu budget elapsed; the dispatch unwound cleanly inside its \
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
                self.respond(slot, result);
                match decide(lifecycle, completion, false) {
                    RequestEnd::Disposed(d) => {
                        self.finish_and_release(slot, entry, d);
                        true
                    }
                    RequestEnd::Parked | RequestEnd::RetainedForStreaming => {
                        self.park_after_slice(slot, )
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
                    // A hard kill is not a trap, or `tombstones` would not
                    // equal `hard_kills + traps`.
                    self.metrics.traps.fetch_add(1, Ordering::Relaxed);
                }
                slot.tombstone();
                self.metrics.tombstones.fetch_add(1, Ordering::Relaxed);
                self.remember_tombstone(slot);
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
                self.respond(slot, result);
                self.finish_and_release(slot, entry, evict());
                true
            }
        }
    }

    /// The instance parks between slices. Returns whether the wake loop is done with it.
    fn park_after_slice(
&self,        slot: &Arc<Slot>,
    ) -> bool {
        slot.park(crate::now_ms());
        // A completion that landed while Hot is drained now.
        !self.rt.futures.has_ready(slot.id)
    }

    fn respond(&self, slot: &Arc<Slot>, result: DispatchResult) {
        self.active.with(&slot.id, |request| {
            request.completion.select_result(&result)
        });
        if let Some(stream) = slot.ctx.responses.lock().transport.clone() {
            if stream.has_started() {
                if let Some(error) = &result.trap {
                    stream.fail(error.clone());
                } else {
                    stream.finish();
                }
            }
        }
        let response = self
            .active
            .with_mut(&slot.id, |request| {
                request
                    .responder
                    .take()
                    .map(|sender| (request.completion.clone(), sender))
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
        completion: Option<Completion>,
    ) -> bool {
        let Some(owner) = self
            .active
            .with(&slot.id, |request| request.completion.clone())
        else {
            return false;
        };
        let stream = slot.ctx.responses.lock().transport.clone();
        let Some(stream) = stream else {
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
            slot.ctx.poison_imports();
            let result = owner.result(slot.id, status, headers, vec![], Completion::Done, None);
            self.respond(slot, result);
            self.finish_and_release(slot, entry, evict());
            return true;
        }
        slot.ctx.poison_imports();
        let status = if failure.is_some() { 502 } else { 499 };
        let terminal = if failure.is_some() {
            TerminalReason::TransportFailure
        } else {
            TerminalReason::RequestCancelled
        };
        self.active.with(&slot.id, |request| {
            request.completion.select(terminal, status)
        });
        let reason = failure.unwrap_or_else(|| "request_cancelled".into());
        let result = owner.result(
            slot.id,
            status,
            vec![],
            vec![],
            Completion::Trapped,
            Some(reason),
        );
        self.respond(slot, result);
        self.finish_and_release(slot, entry, evict());
        true
    }

    pub(super) fn end_failed_capture(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        owner: &Arc<DispatchCompletion>,
        reason: TerminalReason,
    ) {
        owner.select(reason, 500);
        let result = owner.result(
            slot.id,
            500,
            vec![],
            b"dispatch could not survive capture".to_vec(),
            Completion::Trapped,
            Some("capture_failed".into()),
        );
        self.respond(slot, result);
        self.finish_and_release(slot, entry, evict());
    }

    fn finish_and_release(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        d: crate::disposition::Disposition,
    ) {
        let _settlement = slot
            .ctx
            .responses
            .lock()
            .transport
            .take()
            .map(|stream| stream.settlement());
        let completion = self
            .active
            .with(&slot.id, |request| request.completion.clone());
        let _completion = completion.as_ref().map(|owner| owner.settlement());
        slot.ctx.end_http_dispatch();
        let req = self.active.remove(&slot.id);
        let mut abandoned_guest_work = false;
        if let Some(r) = &req {
            abandoned_guest_work = self.rt.futures.end_dispatch(slot.id, r.dispatch_id);
            if let Some(total_wall_ns) = r.completion.total_wall_ns() {
                self.metrics.request_us.record(total_wall_ns / 1_000);
            }
            self.metrics.park_ms.record(slot.parked_ms_total());
            entry.meters.record_park(slot.parked_ms_total());
        }

        let d = if abandoned_guest_work { evict() } else { d };


        if !d.is_warm() && !matches!(d.kind(), crate::disposition::DispositionKind::Reset) {
            self.forget(slot);
        }
        self.rt.futures.release_instance(slot.id);
        entry.pool.release(slot.clone(), d);
        if slot.state() == LifecycleTier::Discarded {
            self.forget(slot);
        }
    }
}
