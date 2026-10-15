//! Waking an idle instance to run its due timers, for an application that
//! opted in during initialization, and stopping one that has gone too long
//! without a request.

use super::*;
use crate::bindings::{DispatchGeneration, IdleTimers};
use queue::Delivery;

/// What an idle instance's watch does when it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleStep {
    /// Run the instance's due timers.
    Wake {
        at: tokio::time::Instant,
        stop_at: tokio::time::Instant,
    },
    /// Stop the instance for want of a request.
    Stop { at: tokio::time::Instant },
}

impl IdleStep {
    /// The next step for an instance that went idle at `now`, or `None` with
    /// no timer pending. A wake waits for the earliest timer, and for the
    /// minimum interval since `now`.
    fn after(
        now: tokio::time::Instant,
        timers: IdleTimers,
        stop_at: tokio::time::Instant,
    ) -> Option<Self> {
        let wake_at = timers.earliest_deadline?.max(now + timers.minimum_interval);
        Some(if wake_at < stop_at {
            IdleStep::Wake {
                at: wake_at,
                stop_at,
            }
        } else {
            IdleStep::Stop { at: stop_at }
        })
    }

    fn at(self) -> tokio::time::Instant {
        match self {
            IdleStep::Wake { at, .. } | IdleStep::Stop { at } => at,
        }
    }
}

impl Cell {
    /// Watches an instance that a dispatch left idle at `generation` for its
    /// next due timer. `stop_at` carries a deadline set by an earlier request;
    /// after a request it is counted from now.
    pub(super) fn watch_idle_timers(
        self: &Arc<Self>,
        slot: &Arc<Slot>,
        entry: &Arc<ModuleEntry>,
        generation: DispatchGeneration,
        stop_at: Option<tokio::time::Instant>,
    ) {
        let Some(timers) = slot.ctx.idle_timers() else {
            return;
        };
        let idle_limit = Duration::from_millis(self.cfg.timer_wake_idle_limit_ms);
        let cell = Arc::downgrade(self);
        let watched = Arc::downgrade(slot);
        let entry = entry.clone();
        self.rt.reactor.spawn(async move {
            let now = tokio::time::Instant::now();
            let Some(step) = IdleStep::after(now, timers, stop_at.unwrap_or(now + idle_limit))
            else {
                return;
            };
            tokio::time::sleep_until(step.at()).await;
            let (Some(cell), Some(slot)) = (cell.upgrade(), watched.upgrade()) else {
                return;
            };
            // A later dispatch watches for itself.
            if slot.ctx.dispatch_generation() == generation {
                cell.idle_timers_due(&entry, &slot, step);
            }
        });
    }

    /// A hibernated instance is not taken, so its timers wait for a request.
    fn idle_timers_due(
        self: &Arc<Self>,
        entry: &Arc<ModuleEntry>,
        slot: &Arc<Slot>,
        step: IdleStep,
    ) {
        let Some(slot) = entry
            .pool
            .take_idle(slot.id, |slot| slot.state() == LifecycleTier::Parked)
        else {
            return;
        };
        match step {
            IdleStep::Wake { stop_at, .. } => self.run_idle_timers(entry, slot, stop_at),
            IdleStep::Stop { .. } => {
                let cell = self.clone();
                let entry = entry.clone();
                self.rt
                    .reactor
                    .spawn_blocking(move || cell.stop_idle(&entry, slot));
            }
        }
    }

    fn run_idle_timers(
        self: &Arc<Self>,
        entry: &Arc<ModuleEntry>,
        slot: Arc<Slot>,
        stop_at: tokio::time::Instant,
    ) {
        let module = entry.activator.module().clone();
        let rec = DispatchRecord {
            dispatch_id: self.next_dispatch.fetch_add(1, Ordering::Relaxed),
            kind: DispatchKind::Timers,
            trace: TraceContext::default(),
            cancel_future: 0,
            occupancy_budget_ms: self.occupancy_budget_ms(&module),
            now_ms: crate::now_ms(),
        };
        let completion = match self.completion_owner(&module, &rec) {
            Ok(completion) => completion,
            Err(error) => {
                tracing::warn!(module = %module, instance = %slot.id, %error,
                    "an idle instance's timers could not be run");
                entry.pool.release(slot, crate::disposition::warm());
                return;
            }
        };
        let job = Box::new(DispatchJob {
            module,
            target: Delivery::IdleTimers {
                slot: slot.clone(),
                stop_at,
            },
            rec,
            responder: dispatch_api::Responder::Nobody,
            stream: None,
            upload: None,
            completion,
        });
        if let Err(job) = self.queue.push_dispatch(job) {
            job.completion
                .reject(completion::AdmissionRejection::QueueUnavailable);
            self.metrics.shed.fetch_add(1, Ordering::Relaxed);
            entry.meters.record_shed();
            // Try again once the interval has passed.
            let generation = slot.ctx.dispatch_generation();
            entry.pool.release(slot.clone(), crate::disposition::warm());
            self.watch_idle_timers(&slot, entry, generation, Some(stop_at));
        }
    }

    fn stop_idle(&self, entry: &Arc<ModuleEntry>, slot: Arc<Slot>) {
        let module = entry.activator.module();
        tracing::info!(target: "isoplane_host::cell::transition",
            event = "instance_transition", tenant = %entry.tenant,
            app = %module.app, version = %module.version,
            instance = slot.id.0, previous = "parked", current = "discarded",
            cause = "no_request_for_timers");
        self.rt.futures.release_instance(slot.id);
        self.forget(&slot);
        entry.pool.release(slot, evict());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timers(
        minimum_interval_ms: u64,
        earliest_deadline: Option<tokio::time::Instant>,
    ) -> IdleTimers {
        IdleTimers {
            minimum_interval: Duration::from_millis(minimum_interval_ms),
            earliest_deadline,
        }
    }

    #[test]
    fn an_idle_instance_is_woken_for_its_timer_no_sooner_than_its_minimum_interval() {
        let now = tokio::time::Instant::now();
        let stop_at = now + Duration::from_secs(300);
        assert_eq!(
            IdleStep::after(
                now,
                timers(1_000, Some(now + Duration::from_millis(10))),
                stop_at
            ),
            Some(IdleStep::Wake {
                at: now + Duration::from_secs(1),
                stop_at
            }),
            "a timer due before the interval has passed waits for it"
        );
        assert_eq!(
            IdleStep::after(
                now,
                timers(1_000, Some(now + Duration::from_secs(5))),
                stop_at
            ),
            Some(IdleStep::Wake {
                at: now + Duration::from_secs(5),
                stop_at
            }),
            "a timer due after the interval wakes the instance when it is due"
        );
        assert_eq!(
            IdleStep::after(now, timers(1_000, None), stop_at),
            None,
            "nothing wakes an instance with no timer pending"
        );
    }

    #[test]
    fn an_idle_instance_is_stopped_rather_than_woken_once_it_has_gone_too_long_without_a_request() {
        let now = tokio::time::Instant::now();
        let stop_at = now + Duration::from_secs(300);
        assert_eq!(
            IdleStep::after(
                now,
                timers(1_000, Some(now + Duration::from_secs(400))),
                stop_at
            ),
            Some(IdleStep::Stop { at: stop_at }),
            "a timer due after the idle limit stops the instance at the limit"
        );
        assert_eq!(
            IdleStep::after(
                now,
                timers(1_000, Some(now)),
                now + Duration::from_millis(500)
            ),
            Some(IdleStep::Stop {
                at: now + Duration::from_millis(500)
            }),
            "a wake the interval would push past the limit stops the instance instead"
        );
    }
}
