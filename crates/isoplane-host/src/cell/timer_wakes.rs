//! Idle background wake deadlines and the five-minute stop without request demand.

use super::*;
use crate::bindings::IdleTimers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleStep {
    Wake {
        at: tokio::time::Instant,
        stop_at: tokio::time::Instant,
    },
    Stop {
        at: tokio::time::Instant,
    },
}

impl IdleStep {
    fn after(timers: IdleTimers, stop_at: tokio::time::Instant) -> Self {
        match timers.earliest_deadline {
            Some(at) if at < stop_at => Self::Wake { at, stop_at },
            Some(_) | None => Self::Stop { at: stop_at },
        }
    }

    fn at(self) -> tokio::time::Instant {
        match self {
            Self::Wake { at, .. } | Self::Stop { at } => at,
        }
    }
}

impl Cell {
    pub(super) fn watch_idle_timers(self: &Arc<Self>, slot: &Arc<Slot>, entry: &Arc<ModuleEntry>) {
        let timers = slot.ctx.idle_timers().or_else(|| {
            (slot.ctx.background.has_work()
                || slot.ctx.has_outbound_http()
                || self.rt.futures.pending_total(slot.id) > 0
                || self.rt.futures.has_background_ready(slot.id))
            .then_some(IdleTimers {
                earliest_deadline: None,
            })
        });
        let (Some(timers), Some(stop_at)) = (timers, slot.ctx.background.idle_stop_at()) else {
            return;
        };
        slot.ctx.report_background();
        let generation = slot.ctx.dispatch_generation();
        let step = if slot.state() == LifecycleTier::Hibernated {
            IdleStep::Stop { at: stop_at }
        } else {
            IdleStep::after(timers, stop_at)
        };
        let cell = Arc::downgrade(self);
        let watched = Arc::downgrade(slot);
        let entry = entry.clone();
        let task = self.rt.reactor.spawn(async move {
            tokio::time::sleep_until(step.at()).await;
            let (Some(cell), Some(slot)) = (cell.upgrade(), watched.upgrade()) else {
                return;
            };
            let reactor = cell.rt.reactor.clone();
            reactor.spawn_blocking(move || {
                let _execution = slot.lock_execution();
                if slot.ctx.dispatch_generation() != generation {
                    return;
                }
                if matches!(step, IdleStep::Wake { .. })
                    && slot.state() == LifecycleTier::Hibernated
                {
                    cell.watch_idle_timers(&slot, &entry);
                    return;
                }
                let Some(idle) = entry.pool.take_idle(slot.id, |slot| {
                    matches!(
                        slot.state(),
                        LifecycleTier::Parked | LifecycleTier::Hibernated
                    )
                }) else {
                    return;
                };
                match step {
                    IdleStep::Stop { .. } => cell.stop_idle(&entry, idle),
                    IdleStep::Wake { .. } => {
                        entry.pool.release(idle, warm());
                        slot.ctx.background.requested.store(true, Ordering::Release);
                        cell.schedule_remaining(&slot);
                    }
                }
            });
        });
        slot.ctx.background.watch_idle(task.abort_handle());
    }

    pub(super) fn stop_idle(&self, entry: &Arc<ModuleEntry>, slot: Arc<Slot>) {
        let module = entry.activator.module();
        tracing::info!(target: "isoplane_host::cell::transition",
            event = "instance_transition", tenant = %entry.tenant,
            app = %module.app, version = %module.version,
            instance = slot.id.0, previous = slot.state().as_str(), current = "discarded",
            cause = "no_request_for_background");
        self.rt.futures.release_instance(slot.id);
        self.forget(&slot);
        entry.pool.release(slot, evict());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_non_timer_work_arms_the_idle_stop() {
        use crate::cell::pool::cell_tests::{app, cell, http, wait_for};
        use crate::fakes::FakeBehaviour;
        for hibernate in [false, true] {
            let cell = cell(CellConfig::for_test("background-idle-stop"));
            let module = app(&cell, "background-idle-stop", FakeBehaviour::done());
            let result = cell
                .dispatch_blocking(
                    &module,
                    Target::Handler { route: "/".into() },
                    http(1),
                    Duration::from_secs(10),
                )
                .expect("request completes");
            assert!(wait_for(
                || cell.idle_instances(&module) == Some(1),
                Duration::from_secs(10)
            ));
            let slot = cell.slot(result.instance).expect("warm instance");
            cell.rt
                .futures
                .alloc(slot.id, crate::futures::INSTANCE_WORK, "http_fetch", 8)
                .expect("retained operation");
            if hibernate {
                cell.force_hibernate(slot.id)
                    .expect("hibernate retained work");
                assert_eq!(slot.state(), LifecycleTier::Hibernated);
            }
            slot.ctx.background.request_arrived(Duration::ZERO);
            let entry = cell.modules.read()[&module].clone();
            cell.watch_idle_timers(&slot, &entry);
            assert!(wait_for(
                || cell.slot(slot.id).is_none(),
                Duration::from_secs(10)
            ));
            assert_eq!(cell.futures().live_futures(), 0);
            cell.shutdown();
        }
    }

    #[test]
    fn the_idle_stop_applies_to_timers_and_non_timer_resources() {
        let now = tokio::time::Instant::now();
        let stop_at = now + Duration::from_secs(300);
        for deadline in [None, Some(stop_at), Some(stop_at + Duration::from_secs(1))] {
            assert_eq!(
                IdleStep::after(
                    IdleTimers {
                        earliest_deadline: deadline
                    },
                    stop_at
                ),
                IdleStep::Stop { at: stop_at }
            );
        }
        let due = now + Duration::from_secs(1);
        assert_eq!(
            IdleStep::after(
                IdleTimers {
                    earliest_deadline: Some(due)
                },
                stop_at
            ),
            IdleStep::Wake { at: due, stop_at }
        );
    }
}
