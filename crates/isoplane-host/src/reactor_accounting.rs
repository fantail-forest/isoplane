//! Attributes active reactor polls through a shared, transferable execution scope.

use std::future::{poll_fn, Future};
use std::pin::pin;
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use crate::meters::{Meters, OccupancyInterval};

pub(crate) trait Observer: Send + Sync {
    fn started(&self);
    fn interval(&self, ns: Option<u64>);
    fn finished(&self);
}

/// Operations and their children follow this scope; an active poll retains its owner.
#[derive(Clone, Default)]
pub(crate) struct Attribution(Arc<Mutex<Option<Weak<dyn Observer>>>>);

impl Attribution {
    pub(crate) fn set(&self, observer: Option<&Arc<dyn Observer>>) {
        *self.0.lock() = observer.map(Arc::downgrade);
    }

    fn begin_poll(&self) -> Option<Arc<dyn Observer>> {
        let owner = self.0.lock();
        let observer = owner.as_ref().and_then(Weak::upgrade);
        if let Some(observer) = &observer {
            // Handover cannot finish the old owner before this poll registers.
            observer.started();
        }
        observer
    }
}

tokio::task_local! {
    static CURRENT: TaskUsage;
}

/// Capture attribution before spawning a transport or resolver child task.
pub(crate) fn inherit(future: impl Future<Output = ()>) -> impl Future<Output = ()> {
    let usage = CURRENT.try_with(Clone::clone).ok();
    async move {
        match usage {
            Some(usage) => usage.measure(future).await,
            None => future.await,
        }
    }
}

#[derive(Clone)]
pub(crate) struct TaskUsage {
    meters: Arc<Meters>,
    attribution: Attribution,
}

impl TaskUsage {
    pub(crate) fn new(meters: Arc<Meters>, attribution: Attribution) -> Self {
        Self {
            meters,
            attribution,
        }
    }

    pub(crate) fn occupancy_ns(&self) -> u64 {
        self.meters.occupancy_ns()
    }

    pub(crate) async fn measure(self, future: impl Future<Output = ()>) {
        CURRENT
            .scope(self.clone(), async {
                let mut future = pin!(future);
                poll_fn(|context| {
                    let _interval = PollUsage {
                        meters: &self.meters,
                        observer: self.attribution.begin_poll(),
                        interval: OccupancyInterval::start(),
                    };
                    future.as_mut().poll(context)
                })
                .await;
            })
            .await;
    }
}

struct PollUsage<'a> {
    meters: &'a Meters,
    observer: Option<Arc<dyn Observer>>,
    interval: OccupancyInterval,
}

impl Drop for PollUsage<'_> {
    fn drop(&mut self) {
        let ns = self.interval.try_elapsed_ns();
        self.meters.record_interval(ns);
        if let Some(observer) = &self.observer {
            observer.interval(ns);
            observer.finished();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    #[derive(Default)]
    struct Observed(std::sync::atomic::AtomicU64);
    impl Observer for Observed {
        fn started(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        fn interval(&self, _: Option<u64>) {}
        fn finished(&self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn child_tasks_keep_attribution_after_the_parent_finishes() {
        let meters = Arc::new(Meters::new());
        let observed = Arc::new(Observed::default());
        let attribution = Attribution::default();
        let observer: Arc<dyn Observer> = observed.clone();
        attribution.set(Some(&observer));
        let task = TaskUsage::new(meters.clone(), attribution);
        let mut child = None;
        let mut parent = Box::pin(task.measure(async {
            child = Some(Box::pin(inherit(std::future::pending())));
        }));
        let mut context = Context::from_waker(Waker::noop());
        crate::occupancy_clock::with_readings([100, 140], || {
            assert_eq!(parent.as_mut().poll(&mut context), Poll::Ready(()));
        });
        drop(parent);
        assert_eq!(observed.0.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(
            CURRENT.try_with(|_| ()).is_err(),
            "attribution must not leak outside the poll"
        );
        let mut child = child.expect("parent created a child task");
        crate::occupancy_clock::with_readings([1_000_000, 1_000_030], || {
            assert_eq!(child.as_mut().poll(&mut context), Poll::Pending);
        });
        assert_eq!(meters.occupancy_ns(), 70);
        drop(child);
        assert_eq!(observed.0.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[test]
    fn only_active_polls_are_measured_and_cancellation_releases_the_task() {
        let meters = Arc::new(Meters::new());
        let observed = Arc::new(Observed::default());
        let attribution = Attribution::default();
        let observer: Arc<dyn Observer> = observed.clone();
        attribution.set(Some(&observer));
        let task = TaskUsage::new(meters.clone(), attribution);
        let mut future = Box::pin(task.measure(std::future::pending()));
        let mut context = Context::from_waker(Waker::noop());
        crate::occupancy_clock::with_readings([100, 140, 1_000_000, 1_000_060], || {
            assert_eq!(future.as_mut().poll(&mut context), Poll::Pending);
            assert_eq!(meters.occupancy_ns(), 40);
            assert_eq!(future.as_mut().poll(&mut context), Poll::Pending);
            assert_eq!(meters.occupancy_ns(), 100, "time between polls is outside active occupancy");
        });
        assert_eq!(observed.0.load(std::sync::atomic::Ordering::Relaxed), 0);
        drop(future);
        assert_eq!(observed.0.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
