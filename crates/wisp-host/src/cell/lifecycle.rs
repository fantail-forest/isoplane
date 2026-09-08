//! One dispatch from admission to its final response: the request lifecycle across every slice, the outcome switch, and the books a tombstone keeps.

use super::*;

impl Cell {
    pub(super) fn run_dispatch(self: &Arc<Self>, job: DispatchJob) {
        let did = job.rec.dispatch_id;
        let entry = self.modules.read().get(&job.module).cloned();
        let Some(entry) = entry else {
            answer(
                &job.responder,
                DispatchResult {
                    status: 404,
                    body: b"no such module".to_vec(),
                    ..DispatchResult::shed(did)
                },
            );
            return;
        };

        let t0 = Instant::now();
        let acquired = match &job.target {
            Target::Handler { .. } => self.acquire_handler(&entry),
        };
        let (slot, src) = match acquired {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, module = %job.module, "acquisition failed");
                answer(
                    &job.responder,
                    DispatchResult {
                        status: 500,
                        body: e.to_string().into_bytes(),
                        ..DispatchResult::shed(did)
                    },
                );
                return;
            }
        };
        let _execution = slot.lock_execution();
        self.note_acquired(src, t0.elapsed());
        let acquired = src.label();

        let budget = CpuBudget {
            cpu_ms: job.rec.cpu_budget_ms,
            wall_ms_cap: self.cfg.wall_ms_cap,
            // Later slices carry what earlier ones spent, so the budget binds
            // the dispatch rather than each wake.
            spent_ns: 0,
        };
        // A Handler never reaches this: a parked instance is not in the pool.
        // A durable instance is addressed by identity and takes one dispatch
        // at a time; serialising its messages is the durability layer's job.
        // Counted before `dispatches`, so a 409 is not a dispatch.
        if self.active.contains_key(&slot.id) {
            self.metrics
                .single_writer_refusals
                .fetch_add(1, Ordering::Relaxed);
            answer(
                &job.responder,
                DispatchResult {
                    status: 409,
                    body: "durable instance already has a dispatch in flight"
                        .as_bytes()
                        .to_vec(),
                    ..DispatchResult::shed(did)
                },
            );
            return;
        }
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
                started_ns: job.enqueued_ns,
                cpu_ns: 0,
                wall_ns: 0,
                slices: 0,
                acquired,
                budget,
            },
        );

        IN_FLIGHT.set(Some((slot.id, job.rec.dispatch_id)));

        entry.meters.record_dispatch();
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);

        slot.ctx
            .begin_dispatch(crate::now_ms(), job.rec.dispatch_id);
        slot.ctx.set_cancel_future(job.rec.cancel_future);

        let outcome = {
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(e) => {
                    self.count_enter_refusal(e);
                    self.respond(
                        &slot,
                        DispatchResult {
                            status: 409,
                            body: b"instance busy".to_vec(),
                            ..DispatchResult::shed(did)
                        },
                    );
                    self.active.remove(&slot.id);
                    return;
                }
            };
            self.hot.fetch_add(1, Ordering::Relaxed);
            let out = guard.instance().dispatch(&job.rec, budget);
            self.hot.fetch_sub(1, Ordering::Relaxed);
            if ends_in_tombstone(&out, &slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            out
        };

        // A completion that landed while the dispatch was Hot waited on the
        // reentrancy rule; hand the instance to the wake lane for it.
        if !self.settle(&slot, &entry, outcome) {
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
        let Some(dispatch_id) = self.active.with(&slot.id, |request| request.dispatch_id) else {
            return;
        };
        IN_FLIGHT.set(Some((slot.id, dispatch_id)));
        let module = self
            .residents
            .lock()
            .get(&slot.id)
            .map(|r| r.module.clone());
        let Some(module) = module else { return };
        let Some(entry) = self.modules.read().get(&module).cloned() else {
            return;
        };

        loop {
            // The instance is entered before the result is taken out, so a
            // result is never pulled with nowhere to deliver it. A busy
            // instance drains the result itself when its call returns.
            let mut guard = match slot.enter() {
                Ok(g) => g,
                Err(e @ (EnterError::Busy | EnterError::Tombstoned)) => {
                    self.count_enter_refusal(e);
                    return;
                }
                Err(EnterError::NotResident(_)) => return,
            };
            let Some((fid, res)) = self.rt.futures.take_ready(slot.id) else {
                // The first wake's loop drained every completion.
                return;
            };
            // The budget binds the dispatch, so a wake resumes with what
            // earlier slices burned; otherwise a guest could park and wake
            // indefinitely on a fresh budget each time.
            let budget = self
                .active
                .with_mut(&slot.id, |a| a.budget.after(a.cpu_ns))
                .unwrap_or_else(CpuBudget::http);

            slot.ctx
                .begin_slice(crate::now_ms(), slot.ctx.dispatch_id());

            let t0 = Instant::now();
            self.hot.fetch_add(1, Ordering::Relaxed);
            let outcome = guard.instance().wake(fid, &res, budget);
            self.hot.fetch_sub(1, Ordering::Relaxed);
            if ends_in_tombstone(&outcome, &slot) {
                guard.destroy();
            } else {
                drop(guard);
            }
            self.metrics.wake_us.record(t0.elapsed().as_micros() as u64);

            if self.settle(&slot, &entry, outcome) {
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
    ) -> bool {
        // The Hot interval, overage included: the meter reads the thread
        // clock and does not know what a budget is.
        entry.meters.record_cpu(&out);
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
                latched_msg = Some(format!("{code:?}"));
                Completion::Trapped
            }
            _ => out.completion,
        };

        let (lifecycle, module, route) = self
            .active
            .with_mut(&slot.id, |req| {
                req.cpu_ns += out.cpu_ns;
                req.wall_ns += out.wall_ns;
                req.slices += 1;
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

        match completion {
            Completion::Parked => self.park_after_slice(slot),
            Completion::Done => {
                // An unfinished streaming response stays in the table for the
                // `resp_write`s that follow `done`. The caller receives the
                // bytes accumulated so far; later chunks are readable through
                // `Cell::response_snapshot` and are not piped to it.
                let response = match out.status {
                    PollStatus::Done { response } if response != 0 => {
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
                let outstanding = self.rt.futures.wait_until_outstanding(slot.id);
                let wait_until = outstanding || streaming_open;

                let (status, headers, body) = match &response {
                    Some(r) => (r.head.status, r.head.headers.clone(), r.body()),
                    None => (204, vec![], Vec::new()),
                };
                let result = self.build_result(slot, status, headers, body, completion, None);
                self.respond(slot, result);

                match decide(lifecycle, completion, wait_until) {
                    RequestEnd::RetainedForWaitUntil => {
                        slot.park(crate::now_ms());
                        !self.rt.futures.has_ready(slot.id)
                    }
                    RequestEnd::Disposed(d) => {
                        self.breaker.record_success(&breaker_key);
                        self.finish_and_release(slot, entry, d);
                        true
                    }
                    RequestEnd::Parked => self.park_after_slice(slot),
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
                let result = self.build_result(
                    slot,
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
                    RequestEnd::Parked | RequestEnd::RetainedForWaitUntil => {
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
                let result = self.build_result(
                    slot,
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
    fn park_after_slice(&self, slot: &Arc<Slot>) -> bool {
        slot.park(crate::now_ms());
        // A completion that landed while Hot is drained now.
        !self.rt.futures.has_ready(slot.id)
    }

    fn build_result(
        &self,
        slot: &Arc<Slot>,
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        completion: Completion,
        trap: Option<String>,
    ) -> DispatchResult {
        let (dispatch_id, cpu_ns, wall_ns, slices, acquired) = self
            .active
            .with(&slot.id, |r| {
                (r.dispatch_id, r.cpu_ns, r.wall_ns, r.slices, r.acquired)
            })
            .unwrap_or((0, 0, 0, 0, "unknown"));
        DispatchResult {
            dispatch_id,
            status,
            headers,
            body,
            completion,
            cpu_ns,
            wall_ns,
            trap,
            instance: slot.id,
            acquired,
            slices,
        }
    }

    fn respond(&self, slot: &Arc<Slot>, result: DispatchResult) {
        let tx = self
            .active
            .with_mut(&slot.id, |a| a.responder.take())
            .flatten();
        if let Some(tx) = tx {
            answer(&tx, result);
        }
    }

    fn finish_and_release(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        d: crate::disposition::Disposition,
    ) {
        let req = self.active.remove(&slot.id);
        if let Some(r) = &req {
            self.rt.futures.end_dispatch(slot.id, r.dispatch_id);
            if let (Some(now), Some(started)) = (crate::mono_ns(), r.started_ns) {
                self.metrics
                    .request_us
                    .record(now.saturating_sub(started) / 1_000);
            }
            self.metrics.park_ms.record(slot.parked_ms_total());
            entry.meters.record_park(slot.parked_ms_total());
        }

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
