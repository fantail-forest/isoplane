//! Connections an instance has in progress. Beyond its limit a further attempt
//! waits, first in, first out, rather than being refused; the guest sees only a
//! slower connect. Waiting is part of the attempt's reactor task, so it occupies
//! no worker, and dropping that task leaves the queue.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Semaphore;

use crate::InstanceId;

/// The largest pending limit the queue can represent.
pub(super) const MAX_PENDING: u64 = Semaphore::MAX_PERMITS as u64;

/// One fair semaphore per instance with an attempt in progress or waiting.
pub(super) struct PendingConnections {
    limit: usize,
    instances: Mutex<HashMap<InstanceId, Arc<Semaphore>>>,
}

/// A place in an instance's queue: waiting until `held`, then in progress until dropped.
pub(super) struct Attempt {
    queues: Arc<PendingConnections>,
    instance: InstanceId,
    semaphore: Arc<Semaphore>,
    held: bool,
}

/// The instance's queue stopped admitting attempts.
#[derive(Debug, thiserror::Error)]
#[error("the outbound connection queue for instance {instance} is closed")]
pub(super) struct QueueClosed {
    instance: InstanceId,
}

impl PendingConnections {
    pub(super) fn new(limit: u64) -> Self {
        Self {
            limit: usize::try_from(limit.min(MAX_PENDING)).unwrap_or(Semaphore::MAX_PERMITS),
            instances: Mutex::new(HashMap::new()),
        }
    }

    /// Waits until `instance` has fewer than its limit of attempts in progress.
    pub(super) async fn begin(
        self: &Arc<Self>,
        instance: InstanceId,
    ) -> Result<Attempt, QueueClosed> {
        let semaphore = self
            .instances
            .lock()
            .entry(instance)
            .or_insert_with(|| Arc::new(Semaphore::new(self.limit)))
            .clone();
        let mut attempt = Attempt {
            queues: self.clone(),
            instance,
            semaphore,
            held: false,
        };
        attempt
            .semaphore
            .acquire()
            .await
            .map_err(|_| QueueClosed { instance })?
            .forget();
        attempt.held = true;
        Ok(attempt)
    }

    #[cfg(test)]
    pub(super) fn tracked(&self) -> usize {
        self.instances.lock().len()
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if self.held {
            self.semaphore.add_permits(1);
        }
        let mut instances = self.queues.instances.lock();
        // Every attempt clones the semaphore under this lock, so the map's copy
        // and this one are the only references when no other attempt exists.
        if Arc::strong_count(&self.semaphore) == 2 {
            instances.remove(&self.instance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll};

    fn poll_once<F: Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(std::task::Waker::noop()))
    }

    #[tokio::test]
    async fn attempts_beyond_the_limit_wait_in_order_and_leave_no_entry() {
        let queues = Arc::new(PendingConnections::new(2));
        let first = queues.begin(InstanceId(1)).await.expect("first slot");
        let second = queues.begin(InstanceId(1)).await.expect("second slot");
        let mut third = pin!(queues.begin(InstanceId(1)));
        let mut fourth = pin!(queues.begin(InstanceId(1)));
        assert!(
            poll_once(third.as_mut()).is_pending(),
            "the third attempt waits"
        );
        assert!(
            poll_once(fourth.as_mut()).is_pending(),
            "the fourth attempt waits"
        );
        let other = queues
            .begin(InstanceId(2))
            .await
            .expect("another instance has its own queue");
        drop(first);
        assert!(
            poll_once(fourth.as_mut()).is_pending(),
            "a later waiter cannot overtake an earlier one"
        );
        let third = match poll_once(third.as_mut()) {
            Poll::Ready(attempt) => attempt.expect("the third attempt starts"),
            Poll::Pending => panic!("a finished attempt admits the first waiter"),
        };
        drop(second);
        let fourth = match poll_once(fourth.as_mut()) {
            Poll::Ready(attempt) => attempt.expect("the fourth attempt starts"),
            Poll::Pending => panic!("the second finished attempt admits the next waiter"),
        };
        drop((third, fourth, other));
        assert_eq!(queues.tracked(), 0, "idle instances keep no queue");
    }

    #[tokio::test]
    async fn a_cancelled_waiter_leaves_the_queue_and_takes_no_slot() {
        let queues = Arc::new(PendingConnections::new(1));
        let running = queues.begin(InstanceId(7)).await.expect("first slot");
        {
            let mut cancelled = pin!(queues.begin(InstanceId(7)));
            assert!(
                poll_once(cancelled.as_mut()).is_pending(),
                "the waiter queues"
            );
        }
        let mut next = pin!(queues.begin(InstanceId(7)));
        assert!(
            poll_once(next.as_mut()).is_pending(),
            "the slot is still in progress"
        );
        drop(running);
        let next = match poll_once(next.as_mut()) {
            Poll::Ready(attempt) => attempt.expect("the slot is free"),
            Poll::Pending => panic!("a cancelled waiter must not take the released slot"),
        };
        drop(next);
        assert_eq!(
            queues.tracked(),
            0,
            "a cancelled waiter leaves no queue behind"
        );
        {
            let blocker = queues.begin(InstanceId(8)).await.expect("slot");
            let mut cancelled = pin!(queues.begin(InstanceId(8)));
            assert!(poll_once(cancelled.as_mut()).is_pending());
            drop(blocker);
        }
        assert_eq!(
            queues.tracked(),
            0,
            "a waiter cancelled after its slot was released leaves no queue behind"
        );
    }
}
