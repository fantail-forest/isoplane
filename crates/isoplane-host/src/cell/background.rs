//! Background slices use the instance allowance and the ordinary execution queue.

use super::*;
use crate::futures::INSTANCE_WORK;

impl Cell {
    fn background_ready(&self, slot: &Slot) -> bool {
        slot.ctx.background.requested.load(Ordering::Acquire)
            || self.rt.futures.has_background_ready(slot.id)
    }

    pub(super) fn schedule_remaining(self: &Arc<Self>, slot: &Arc<Slot>) {
        if slot.state() == LifecycleTier::Discarded {
            return;
        }
        if self.has_starts(slot.id) || self.has_runnable(slot) {
            self.queue_wake(slot.clone());
        }
        if !self.background_ready(slot) || slot.state() == LifecycleTier::Hibernated {
            return;
        }
        match slot.ctx.background.allowance() {
            Ok(_) => self.queue_wake(slot.clone()),
            Err(at) => {
                if slot
                    .ctx
                    .background
                    .refill_watch
                    .swap(true, Ordering::AcqRel)
                {
                    return;
                }
                slot.ctx.report_background();
                let cell = Arc::downgrade(self);
                let watched = Arc::downgrade(slot);
                self.rt.reactor.spawn(async move {
                    tokio::time::sleep_until(at).await;
                    let (Some(cell), Some(slot)) = (cell.upgrade(), watched.upgrade()) else {
                        return;
                    };
                    slot.ctx
                        .background
                        .refill_watch
                        .store(false, Ordering::Release);
                    cell.schedule_remaining(&slot);
                });
            }
        }
    }

    /// The caller owns the execution lock, including pool acquisition and release.
    pub(super) fn run_background(self: &Arc<Self>, slot: &Arc<Slot>) -> bool {
        if !self.background_ready(slot) || slot.state() != LifecycleTier::Parked {
            return false;
        }
        let Ok(allowance) = slot.ctx.background.allowance() else {
            return false;
        };
        let entry = self
            .first_active(slot.id, |active| active.completion.entry.clone())
            .or_else(|| self.modules.read().get(&slot.ctx.module).cloned());
        let Some(entry) = entry else {
            return false;
        };
        if !self.active.contains_key(&slot.id) {
            if entry
                .pool
                .take_idle(slot.id, |slot| slot.state() == LifecycleTier::Parked)
                .is_none()
            {
                return false;
            }
            if slot
                .ctx
                .background
                .idle_stop_at()
                .is_some_and(|at| at <= slot.ctx.background.now())
            {
                self.stop_idle(&entry, slot.clone());
                return true;
            }
        }
        let budget_ms = allowance.as_nanos().div_ceil(1_000_000) as u32;
        let budget = OccupancyBudget {
            occupancy_ms: budget_ms,
            slice_wall_ms_cap: self.cfg.slice_wall_ms_cap,
            spent_ns: u64::from(budget_ms) * 1_000_000 - allowance.as_nanos() as u64,
        };
        let record = DispatchRecord {
            dispatch_id: INSTANCE_WORK,
            kind: DispatchKind::Background,
            trace: TraceContext::default(),
            cancel_future: 0,
            occupancy_budget_ms: budget_ms,
            pending_futures_limit: slot.ctx.pending_futures_limit(INSTANCE_WORK),
            now_ms: crate::now_ms(),
        };
        let owner = match DispatchCompletion::new(
            entry.clone(),
            &record,
            budget,
            self.completion_clock.read().clone(),
        ) {
            Ok(owner) => owner,
            Err(error) => {
                tracing::error!(instance = %slot.id, %error, "background execution could not acquire accounting");
                if !self.active.contains_key(&slot.id) {
                    entry.pool.release(slot.clone(), warm());
                }
                return false;
            }
        };
        owner.acquired(slot.id, "background");
        self.waited_for_thread(&owner, slot.ctx.background.take_thread_wait());
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);
        self.insert_active(
            slot.id,
            ActiveDispatch {
                dispatch_id: INSTANCE_WORK,
                module: slot.ctx.module.clone(),
                route: String::new(),
                grace_deadline: None,
                responder: None,
                completion: owner.clone(),
            },
            false,
        );
        self.share(&entry, slot);
        slot.ctx
            .background
            .requested
            .store(false, Ordering::Release);
        slot.ctx.background.ran_background(true);
        let dispatch = slot.ctx.start_dispatch(crate::now_ms(), INSTANCE_WORK);
        let observer: Arc<dyn crate::reactor_accounting::Observer> = slot.ctx.background.clone();
        dispatch.reactor_attribution.set(Some(&observer));
        IN_FLIGHT.replace(Some(Arc::downgrade(&owner)));
        owner.in_scope(|| {
            let mut guard = match slot.enter() {
                Ok(guard) => guard,
                Err(_) => {
                    self.fail_instance(
                        slot,
                        &entry,
                        TerminalReason::EntryRefused,
                        409,
                        "background_entry_refused",
                    );
                    return;
                }
            };
            let ready = self.rt.futures.take_background_ready(slot.id);
            let (future, result) = ready.map_or(
                (0, isoplane_abi::result::ResultRecord::Ok(Vec::new())),
                |ready| (ready.future, ready.result),
            );
            let hot = super::completion::HotSlice::new(&self.hot);
            owner.begin_slice();
            let outcome = guard
                .instance()
                .wake(INSTANCE_WORK, future, &result, budget);
            drop(hot);
            owner.add_slice(&outcome);
            entry.meters.record_occupancy(&outcome);
            self.metrics.slice_us.record(outcome.wall_ns / 1_000);
            self.metrics
                .grace_deferrals
                .fetch_add(outcome.grace_deferrals, Ordering::Relaxed);
            if outcome.completion == Completion::BudgetExit {
                entry
                    .meters
                    .record_grace_end(GraceEnd::Exited, outcome.overage_ns);
                self.metrics.budget_exits.fetch_add(1, Ordering::Relaxed);
                self.metrics.grace_entries.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .overage_ns
                    .fetch_add(outcome.overage_ns, Ordering::Relaxed);
            }
            slot.ctx
                .background
                .charge(Duration::from_nanos(outcome.occupancy_ns));
            if ends_the_instance(&outcome, slot) || slot.ctx.has_trap() {
                guard.destroy();
                let reason = if outcome.completion == Completion::BudgetKill {
                    entry
                        .meters
                        .record_grace_end(GraceEnd::Killed, outcome.overage_ns);
                    self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
                    self.metrics
                        .overage_ns
                        .fetch_add(outcome.overage_ns, Ordering::Relaxed);
                    if outcome.grace_end.is_some() {
                        self.metrics.grace_entries.fetch_add(1, Ordering::Relaxed);
                    }
                    match outcome.execution_limit {
                        Some(isoplane_engine::ExecutionLimit::Wall) => TerminalReason::WallLimit,
                        Some(isoplane_engine::ExecutionLimit::Occupancy) | None => {
                            TerminalReason::OccupancyLimit
                        }
                    }
                } else {
                    entry.meters.record_trap();
                    self.metrics.traps.fetch_add(1, Ordering::Relaxed);
                    TerminalReason::GuestTrap
                };
                self.fail_instance(slot, &entry, reason, 500, "background_execution_failed");
            } else {
                self.retire_past_memory_limit(slot, &entry, guard.instance().memory_len());
                drop(guard);
                let (reason, status) = match outcome.completion {
                    Completion::Failed => (TerminalReason::BackgroundFailed, 500),
                    Completion::Cancelled => (TerminalReason::BackgroundCancelled, 499),
                    Completion::Done | Completion::Parked | Completion::BudgetExit => {
                        (TerminalReason::Success, 204)
                    }
                    Completion::Trapped => (TerminalReason::GuestTrap, 500),
                    Completion::BudgetKill => (TerminalReason::OccupancyLimit, 503),
                };
                owner.select(reason, status);
                self.finish_and_release(slot, &entry, INSTANCE_WORK, warm());
            }
            slot.ctx.report_background();
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::completion::test_events::Events;
    use crate::cell::pool::cell_tests::{app, cell, http};
    use crate::fakes::{FakeBehaviour, Step};
    use isoplane_abi::status::PollStatus;

    #[test]
    fn background_failures_are_reported_without_discarding_the_instance() {
        for (outcome, reason, status) in [
            (PollStatus::Failed, "background_failed", 500),
            (PollStatus::Cancelled, "background_cancelled", 499),
        ] {
            let events = Events::default();
            let _trace = events.install();
            let cell = cell(CellConfig {
                exec_threads: 0,
                ..CellConfig::for_test("background-outcome")
            });
            let module = app(
                &cell,
                "background-outcome",
                FakeBehaviour::program([
                    Step::Done {
                        status: 200,
                        body: vec![],
                    },
                    Step::Terminal(outcome),
                ]),
            );
            let response = cell
                .submit(&module, Target::Handler { route: "/".into() }, http(1))
                .expect("admission");
            let Job::Dispatch(job) = cell.queue.pop(0).expect("request") else {
                panic!("request job");
            };
            cell.run_dispatch(*job);
            cell.queue.job_done(0);
            let slot = cell
                .slot(response.recv().expect("response").instance)
                .expect("warm instance");
            slot.ctx.background.requested.store(true, Ordering::Release);
            let _execution = slot.lock_execution();
            assert!(cell.run_background(&slot));
            let records = events.records();
            assert_eq!(records.len(), 2);
            assert_eq!(records[1]["dispatch_kind"], "background");
            assert_eq!(records[1]["outcome"], reason);
            assert_eq!(records[1]["status"], status);
            let tenant = cell.metrics().tenants["acme"];
            assert_eq!(
                tenant.dispatches, 2,
                "one request and one background dispatch"
            );
            assert_eq!(
                tenant.traps, 0,
                "background failure and cancellation are not guest traps"
            );
            assert_eq!(slot.state(), LifecycleTier::Parked);
            drop(_execution);
            cell.shutdown();
        }
    }
}
