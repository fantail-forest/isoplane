//! Attributes active reactor polls to their originating dispatch and tenant.

use std::future::{poll_fn, Future};
use std::pin::pin;
use std::sync::Arc;

use crate::meters::{Meters, OccupancyInterval};

pub(crate) trait Observer: Send + Sync {
    fn started(&self);
    fn interval(&self, ns: Option<u64>);
    fn finished(&self);
}

#[derive(Clone)]
struct Attribution {
    meters: Arc<Meters>,
    observer: Option<Arc<dyn Observer>>,
}

tokio::task_local! {
    static CURRENT: Attribution;
}

/// Capture attribution before spawning a transport or resolver child task.
pub(crate) fn inherit(future: impl Future<Output = ()>) -> impl Future<Output = ()> {
    let usage = CURRENT
        .try_with(|owner| TaskUsage::new(owner.meters.clone(), owner.observer.clone()))
        .ok();
    async move {
        match usage {
            Some(usage) => usage.measure(future).await,
            None => future.await,
        }
    }
}

pub(crate) struct TaskUsage {
    meters: Arc<Meters>,
    observer: Option<Arc<dyn Observer>>,
}

impl TaskUsage {
    pub(crate) fn new(meters: Arc<Meters>, observer: Option<Arc<dyn Observer>>) -> Self {
        if let Some(observer) = &observer {
            observer.started();
        }
        Self { meters, observer }
    }

    pub(crate) async fn measure(self, future: impl Future<Output = ()>) {
        let attribution = Attribution {
            meters: self.meters.clone(),
            observer: self.observer.clone(),
        };
        CURRENT
            .scope(attribution, async {
                let mut future = pin!(future);
                poll_fn(|context| {
                    let _interval = PollUsage {
                        task: &self,
                        interval: OccupancyInterval::start(),
                    };
                    future.as_mut().poll(context)
                })
                .await;
            })
            .await;
    }
}

impl Drop for TaskUsage {
    fn drop(&mut self) {
        if let Some(observer) = &self.observer {
            observer.finished();
        }
    }
}

struct PollUsage<'a> {
    task: &'a TaskUsage,
    interval: OccupancyInterval,
}

impl Drop for PollUsage<'_> {
    fn drop(&mut self) {
        let ns = self.interval.try_elapsed_ns();
        self.task.meters.record_interval(ns);
        if let Some(observer) = &self.task.observer {
            observer.interval(ns);
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
        let task = TaskUsage::new(meters.clone(), Some(observed.clone()));
        let mut child = None;
        let mut parent = Box::pin(task.measure(async {
            child = Some(Box::pin(inherit(std::future::pending())));
        }));
        let mut context = Context::from_waker(Waker::noop());
        crate::occupancy_clock::with_readings([100, 140], || {
            assert_eq!(parent.as_mut().poll(&mut context), Poll::Ready(()));
        });
        drop(parent);
        assert_eq!(observed.0.load(std::sync::atomic::Ordering::Relaxed), 1);
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
        let task = TaskUsage::new(meters.clone(), Some(observed.clone()));
        let mut future = Box::pin(task.measure(std::future::pending()));
        let mut context = Context::from_waker(Waker::noop());
        crate::occupancy_clock::with_readings([100, 140, 1_000_000, 1_000_060], || {
            assert_eq!(future.as_mut().poll(&mut context), Poll::Pending);
            assert_eq!(meters.occupancy_ns(), 40);
            assert_eq!(future.as_mut().poll(&mut context), Poll::Pending);
            assert_eq!(meters.occupancy_ns(), 100, "time between polls is outside active occupancy");
        });
        assert_eq!(observed.0.load(std::sync::atomic::Ordering::Relaxed), 1);
        drop(future);
        assert_eq!(observed.0.load(std::sync::atomic::Ordering::Relaxed), 0);
    }
}
