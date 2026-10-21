//! Instance background occupancy: bounded credit, request donations and retained debt.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::time::Instant;

use crate::bindings::WakeSink;
use crate::InstanceId;

const INITIAL_NS: i64 = 5_000_000_000;
const CAPACITY_NS: i64 = 30_000_000_000;
// Five seconds of occupancy per thirty seconds of elapsed host time.
const REFILL_DIVISOR: u64 = 6;

struct Credit {
    balance_ns: i64,
    updated: Instant,
    refill_remainder_ns: u64,
    consumed_ns: u64,
    unmeasured_intervals: u64,
    refilled_ns: u64,
    donated_ns: u64,
    throttled_since: Option<Instant>,
    eligible_since: Option<Instant>,
    throttled: Duration,
    last_was_background: bool,
    idle_stop_at: Option<Instant>,
}

impl Credit {
    fn new(now: Instant) -> Self {
        Self {
            balance_ns: INITIAL_NS,
            updated: now,
            refill_remainder_ns: 0,
            consumed_ns: 0,
            unmeasured_intervals: 0,
            refilled_ns: 0,
            donated_ns: 0,
            throttled_since: None,
            eligible_since: None,
            throttled: Duration::ZERO,
            last_was_background: false,
            idle_stop_at: None,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed_ns = now.saturating_duration_since(self.updated).as_nanos();
        let available_ns = elapsed_ns + u128::from(self.refill_remainder_ns);
        let earned_ns = (available_ns / u128::from(REFILL_DIVISOR)).min(i64::MAX as u128) as i64;
        self.refill_remainder_ns = (available_ns % u128::from(REFILL_DIVISOR)) as u64;
        let accepted_ns = earned_ns.min(CAPACITY_NS.saturating_sub(self.balance_ns));
        self.balance_ns = self.balance_ns.saturating_add(accepted_ns);
        self.refilled_ns = self.refilled_ns.saturating_add(accepted_ns as u64);
        self.updated = now;
        self.resume_if_ready(now);
    }

    fn resume_if_ready(&mut self, now: Instant) {
        if self.balance_ns >= INITIAL_NS {
            if let Some(since) = self.throttled_since.take() {
                self.throttled += now.saturating_duration_since(since);
            }
        }
    }
}

pub(crate) struct Background {
    credit: Mutex<Credit>,
    instance: InstanceId,
    wake: Option<Arc<dyn WakeSink>>,
    reactor: Option<tokio::runtime::Handle>,
    pub(crate) requested: AtomicBool,
    pub(crate) refill_watch: AtomicBool,
    active_polls: AtomicUsize,
    idle_watch: Mutex<Option<tokio::task::AbortHandle>>,
    changed: tokio::sync::Notify,
}

impl Background {
    pub(crate) fn new(
        instance: InstanceId,
        runtime: Option<&crate::bindings::CellRuntime>,
    ) -> Self {
        let _clock = runtime.map(|runtime| runtime.reactor.enter());
        Self {
            credit: Mutex::new(Credit::new(Instant::now())),
            instance,
            wake: runtime.map(|runtime| runtime.wake.clone()),
            reactor: runtime.map(|runtime| runtime.reactor.clone()),
            requested: AtomicBool::new(false),
            refill_watch: AtomicBool::new(false),
            active_polls: AtomicUsize::new(0),
            idle_watch: Mutex::new(None),
            changed: tokio::sync::Notify::new(),
        }
    }

    pub(crate) fn now(&self) -> Instant {
        let _clock = self.reactor.as_ref().map(tokio::runtime::Handle::enter);
        Instant::now()
    }

    pub(crate) fn watch_idle(&self, task: tokio::task::AbortHandle) {
        if let Some(previous) = self.idle_watch.lock().replace(task) {
            previous.abort();
        }
    }

    pub(crate) fn request_arrived(&self, idle_limit: Duration) {
        self.credit.lock().idle_stop_at = Some(self.now() + idle_limit);
    }

    pub(crate) fn idle_stop_at(&self) -> Option<Instant> {
        self.credit.lock().idle_stop_at
    }

    pub(crate) fn last_was_background(&self) -> bool {
        self.credit.lock().last_was_background
    }

    pub(crate) fn ran_background(&self, background: bool) {
        self.credit.lock().last_was_background = background;
    }

    pub(crate) fn has_work(&self) -> bool {
        self.requested.load(Ordering::Acquire) || self.active_polls.load(Ordering::Acquire) > 0
    }

    pub(crate) fn allowance(&self) -> Result<Duration, Instant> {
        let now = self.now();
        let mut credit = self.credit.lock();
        credit.refill(now);
        if credit.balance_ns >= INITIAL_NS {
            Ok(Duration::from_nanos(credit.balance_ns as u64))
        } else {
            credit.throttled_since.get_or_insert(now);
            let delay_ns = INITIAL_NS.saturating_sub(credit.balance_ns) as u64;
            let delay_ns = delay_ns.saturating_mul(REFILL_DIVISOR);
            Err(now + Duration::from_nanos(delay_ns))
        }
    }

    pub(crate) fn queued(&self) {
        let now = self.now();
        let mut credit = self.credit.lock();
        credit.refill(now);
        if credit.balance_ns >= INITIAL_NS {
            credit.eligible_since.get_or_insert(now);
        }
    }

    pub(crate) fn take_thread_wait(&self) -> Duration {
        self.credit
            .lock()
            .eligible_since
            .take()
            .map_or(Duration::ZERO, |at| {
                self.now().saturating_duration_since(at)
            })
    }

    pub(crate) fn charge(&self, occupied: Duration) {
        let mut credit = self.credit.lock();
        credit.refill(self.now());
        let ns = occupied.as_nanos().min(i64::MAX as u128) as i64;
        credit.balance_ns = credit.balance_ns.saturating_sub(ns);
        if credit.balance_ns < INITIAL_NS {
            credit.eligible_since = None;
        }
        credit.consumed_ns = credit.consumed_ns.saturating_add(ns as u64);
    }

    pub(crate) fn donate(&self, unused: Duration) {
        let mut credit = self.credit.lock();
        credit.refill(self.now());
        let ns = (unused.as_nanos().min(i64::MAX as u128) as i64)
            .min(CAPACITY_NS.saturating_sub(credit.balance_ns));
        credit.balance_ns = credit.balance_ns.saturating_add(ns);
        credit.donated_ns = credit.donated_ns.saturating_add(ns as u64);
        credit.resume_if_ready(self.now());
        drop(credit);
        self.changed.notify_waiters();
        if self.requested.load(Ordering::Acquire) || self.refill_watch.load(Ordering::Acquire) {
            if let Some(wake) = &self.wake {
                wake.signal(self.instance);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn consumed_ns(&self) -> u64 {
        self.credit.lock().consumed_ns
    }

    pub(crate) fn report(&self, tenant: &str, module: &crate::ModuleId, retained_connections: u64) {
        let credit = self.credit.lock();
        tracing::info!(event = "background_accounting", tenant, app = %module.app,
            version = %module.version, instance = self.instance.0, retained_connections,
            occupancy_ns = credit.consumed_ns, refilled_ns = credit.refilled_ns,
            unmeasured_intervals = credit.unmeasured_intervals,
            donated_ns = credit.donated_ns, balance_ns = credit.balance_ns,
            capacity_ns = CAPACITY_NS, resume_balance_ns = INITIAL_NS, refill_occupancy_ns = 1_000_000_000u64,
            refill_interval_ns = 1_000_000_000 * REFILL_DIVISOR,
            throttled_ns = credit.throttled.as_nanos() as u64,
            "instance background allowance");
    }
}

impl crate::reactor_accounting::Observer for Background {
    fn wait_for_allowance(self: Arc<Self>) -> Option<crate::reactor_accounting::AllowanceWait> {
        self.allowance().err()?;
        Some(Box::pin(async move {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let Err(at) = self.allowance() else {
                    return;
                };
                tokio::select! {
                    () = tokio::time::sleep_until(at) => {},
                    () = changed => {},
                }
            }
        }))
    }

    fn started(&self) {
        self.active_polls.fetch_add(1, Ordering::AcqRel);
    }

    fn interval(&self, ns: Option<u64>) {
        if let Some(ns) = ns {
            self.charge(Duration::from_nanos(ns));
        } else {
            let mut credit = self.credit.lock();
            credit.unmeasured_intervals = credit.unmeasured_intervals.saturating_add(1);
        }
    }

    fn finished(&self) {
        self.active_polls.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if let Some(task) = self.idle_watch.get_mut().take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn refill_and_donations_repay_debt_without_exceeding_the_credit_cap() {
        let pool = Background::new(InstanceId(1), None);
        assert_eq!(pool.allowance(), Ok(Duration::from_secs(5)));
        pool.queued();
        pool.charge(Duration::from_millis(10));
        let eligible_at = Instant::now() + Duration::from_millis(60);
        assert_eq!(pool.allowance(), Err(eligible_at));
        pool.queued();
        assert_eq!(pool.take_thread_wait(), Duration::ZERO);
        tokio::time::advance(Duration::from_millis(60)).await;
        assert_eq!(pool.allowance(), Ok(Duration::from_secs(5)));
        pool.queued();
        pool.charge(Duration::from_secs(6));
        assert_eq!(pool.credit.lock().balance_ns, -1_000_000_000);
        assert!(pool.allowance().is_err());
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(pool.allowance().is_err());
        assert_eq!(pool.credit.lock().balance_ns, -500_000_000);
        pool.queued();
        assert_eq!(
            pool.take_thread_wait(),
            Duration::ZERO,
            "allowance delay is not thread wait"
        );
        pool.donate(Duration::from_secs(1));
        assert!(
            pool.allowance().is_err(),
            "positive credit alone does not resume an exhausted pool"
        );
        pool.donate(Duration::from_millis(4500));
        pool.queued();
        tokio::time::advance(Duration::from_nanos(3)).await;
        pool.queued();
        assert_eq!(
            pool.take_thread_wait(),
            Duration::from_nanos(3),
            "coalescing preserves eligibility time"
        );
        assert_eq!(pool.allowance(), Ok(Duration::from_secs(5)));
        pool.donate(Duration::from_secs(60));
        assert_eq!(pool.allowance(), Ok(Duration::from_secs(30)));
        tokio::time::advance(Duration::from_secs(300)).await;
        assert_eq!(pool.allowance(), Ok(Duration::from_secs(30)));
        pool.charge(Duration::from_secs(31));
        tokio::time::advance(Duration::from_secs(35)).await;
        assert!(pool.allowance().is_err());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(pool.allowance(), Ok(Duration::from_secs(5)));
        assert_eq!(
            Background::new(InstanceId(2), None).allowance(),
            Ok(Duration::from_secs(5))
        );
    }
    #[tokio::test(start_paused = true)]
    async fn background_reactor_polls_wait_for_credit_and_resume_on_a_donation() {
        use crate::reactor_accounting::{Attribution, Observer, TaskUsage};
        use std::future::{poll_fn, Future};
        use std::task::{Context, Poll, Waker};
        let pool = Arc::new(Background::new(InstanceId(1), None));
        pool.charge(Duration::from_secs(6));
        let attribution = Attribution::default();
        let observer: Arc<dyn Observer> = pool.clone();
        attribution.set(Some(&observer));
        let meters = Arc::new(crate::meters::Meters::new());
        let polls = std::cell::Cell::new(0);
        let mut operation = Box::pin(TaskUsage::new(meters.clone(), attribution).measure(poll_fn(
            |_| {
                polls.set(polls.get() + 1);
                Poll::<()>::Pending
            },
        )));
        let mut context = Context::from_waker(Waker::noop());
        assert_eq!(operation.as_mut().poll(&mut context), Poll::Pending);
        assert_eq!(polls.get(), 0, "no host operation runs while in debt");
        pool.donate(Duration::from_secs(6));
        crate::occupancy_clock::with_readings([100, 130], || {
            assert_eq!(operation.as_mut().poll(&mut context), Poll::Pending);
        });
        assert_eq!(polls.get(), 1);
        assert_eq!(meters.occupancy_ns(), 30);
        assert_eq!(pool.credit.lock().balance_ns, 5_000_000_000 - 30);
        pool.interval(None);
        assert_eq!(pool.credit.lock().unmeasured_intervals, 1);
        assert_eq!(pool.credit.lock().balance_ns, 5_000_000_000 - 30);
    }
}
