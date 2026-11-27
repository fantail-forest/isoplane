use super::*;
use crate::futures::INSTANCE_WORK;

/// How [`Cell::enter_background`] ended.
enum BackgroundEntry {
    Began(Arc<DispatchCompletion>),
    /// The instance was idle past its limit and is discarded.
    Stopped,
    /// The instance left the pool for something else, or the work's
    /// accounting could not begin.
    Unavailable,
}

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
        let budget_ms = allowance.as_nanos().div_ceil(1_000_000) as u32;
        let budget = OccupancyBudget {
            occupancy_ms: budget_ms,
            slice_wall_ms_cap: self.cfg.slice_wall_ms_cap,
            spent_ns: u64::from(budget_ms) * 1_000_000 - allowance.as_nanos() as u64,
        };
        let record = DispatchRecord {
            dispatch_id: INSTANCE_WORK,
            kind: DispatchKind::Background,
            cancel_future: 0,
            occupancy_budget_ms: budget_ms,
            pending_futures_limit: slot.ctx.pending_futures_limit(INSTANCE_WORK),
            now_ms: crate::now_ms(),
        };
        let owner = match self.enter_background(slot, &entry, &record, budget) {
            BackgroundEntry::Began(owner) => owner,
            BackgroundEntry::Stopped => return true,
            BackgroundEntry::Unavailable => return false,
        };
        owner.acquired(slot.id, "background");
        self.waited_for_thread(&owner, slot.ctx.background.take_thread_wait());
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);
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
            owner.begin_slice();
            let outcome = guard
                .instance()
                .wake(INSTANCE_WORK, future, &result, budget);
            owner.add_slice(&outcome);
            entry.meters.record_occupancy(&outcome);
            #[cfg(any(test, feature = "test-util"))]
            if outcome.completion == Completion::BudgetExit {
                entry
                    .meters
                    .record_grace_end(isoplane_engine::GraceEnd::Exited, outcome.overage_ns);
                self.metrics.budget_exits.fetch_add(1, Ordering::Relaxed);
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
                    #[cfg(any(test, feature = "test-util"))]
                    {
                        entry.meters.record_grace_end(
                            isoplane_engine::GraceEnd::Killed,
                            outcome.overage_ns,
                        );
                        self.metrics.budget_kills.fetch_add(1, Ordering::Relaxed);
                        self.metrics
                            .overage_ns
                            .fetch_add(outcome.overage_ns, Ordering::Relaxed);
                    }
                    match outcome.execution_limit {
                        Some(isoplane_engine::ExecutionLimit::Wall) => TerminalReason::WallLimit,
                        Some(isoplane_engine::ExecutionLimit::Occupancy) | None => {
                            TerminalReason::OccupancyLimit
                        }
                    }
                } else {
                    self.metrics.traps.fetch_add(1, Ordering::Relaxed);
                    TerminalReason::GuestTrap
                };
                self.fail_instance(slot, reason, 500, "background_execution_failed");
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

    /// Takes an idle instance from the pool for background work and lists it
    /// as running in the same step, so a dispatch arriving meanwhile joins it
    /// instead of activating another.
    fn enter_background(
        &self,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        record: &DispatchRecord,
        budget: OccupancyBudget,
    ) -> BackgroundEntry {
        let mut shared = entry.shared.lock();
        let idle = !self.active.contains_key(&slot.id);
        if idle {
            if entry
                .pool
                .take_idle(slot.id, |slot| slot.state() == LifecycleTier::Parked)
                .is_none()
            {
                return BackgroundEntry::Unavailable;
            }
            if slot
                .ctx
                .background
                .idle_stop_at()
                .is_some_and(|at| at <= slot.ctx.background.now())
            {
                drop(shared);
                self.stop_idle(entry, slot.clone());
                return BackgroundEntry::Stopped;
            }
        }
        let owner = match DispatchCompletion::new(
            entry.clone(),
            record,
            budget,
            self.completion_clock.read().clone(),
        ) {
            Ok(owner) => owner,
            Err(error) => {
                tracing::error!(instance = %slot.id, %error, "background execution could not acquire accounting");
                if idle {
                    self.release_to_pool(entry, slot);
                }
                return BackgroundEntry::Unavailable;
            }
        };
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
        Self::list_shared(&mut shared, slot);
        BackgroundEntry::Began(owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::pool::cell_tests::{app, cell, http};
    use crate::fakes::{FakeBehaviour, Step};
    use isoplane_abi::status::PollStatus;

    #[test]
    fn a_request_arriving_as_its_idle_instance_starts_background_work_is_served_there() {
        use isoplane_test_support::wait_for;
        let cell = cell(crate::fakes::cell_config("background-placement"));
        let module = app(&cell, "background-placement", FakeBehaviour::done());
        let request = |id| {
            cell.dispatch_blocking(
                &module,
                Target::Handler { route: "/".into() },
                http(id),
                Duration::from_secs(10),
            )
            .expect("the request completes")
        };
        let first = request(1);
        let slot = cell.slot(first.instance).expect("warm instance");
        for id in 2..=200 {
            assert!(
                wait_for(
                    || cell.idle_instances(&module) == Some(1)
                        && !slot.ctx.background.requested.load(Ordering::Acquire),
                    Duration::from_secs(10)
                ),
                "the instance must be idle before request {id}"
            );
            slot.ctx.background.requested.store(true, Ordering::Release);
            cell.schedule_remaining(&slot);
            let result = request(id);
            assert_eq!(
                result.instance, first.instance,
                "request {id} must be served by the idle instance, not one acquired as {}",
                result.acquired
            );
        }
        cell.shutdown();
    }

    #[test]
    fn background_failures_are_reported_without_discarding_the_instance() {
        for (outcome, reason, status) in [
            (PollStatus::Failed, "background_failed", 500),
            (PollStatus::Cancelled, "background_cancelled", 499),
        ] {
            let cell = cell(CellConfig {
                exec_threads: 0,
                ..crate::fakes::cell_config("background-outcome")
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
            assert_eq!(
                *cell.metrics_raw().dispatch_outcomes.lock(),
                std::collections::BTreeMap::from([(("success", 200), 1), ((reason, status), 1)]),
                "the request and the background work each end once, the work with its failure"
            );
            assert_eq!(slot.state(), LifecycleTier::Parked);
            drop(_execution);
            cell.shutdown();
        }
    }
}
