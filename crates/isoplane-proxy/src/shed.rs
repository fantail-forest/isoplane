//! Admission control: `max_in_flight` dispatches may be hot in the cells at
//! once, and `max_queued` callers may wait for a slot before the proxy
//! refuses. Without the second knob an overloaded proxy turns a CPU shortage
//! into an unbounded queue where every request times out having held a
//! thread and a socket.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::metrics::ProxyMetrics;

#[derive(Debug, Clone, Copy)]
pub struct ShedConfig {
    pub max_in_flight: usize,
    pub max_queued: usize,
}

impl Default for ShedConfig {
    fn default() -> Self {
        ShedConfig {
            max_in_flight: 256,
            max_queued: 1024,
        }
    }
}

pub enum Admit {
    /// Held for the dispatch; dropping it frees the slot.
    Granted { _permit: OwnedSemaphorePermit },
    /// Refused at the edge: answer 503 with `Retry-After`.
    Shed,
}

pub struct AdmissionControl {
    sem: Arc<Semaphore>,
    queued: AtomicUsize,
    cfg: ShedConfig,
    metrics: Arc<ProxyMetrics>,
}

struct QueueReservation<'a>(&'a AtomicUsize);

impl Drop for QueueReservation<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl AdmissionControl {
    pub fn new(cfg: ShedConfig, metrics: Arc<ProxyMetrics>) -> Self {
        AdmissionControl {
            sem: Arc::new(Semaphore::new(cfg.max_in_flight)),
            queued: AtomicUsize::new(0),
            cfg,
            metrics,
        }
    }

    pub fn in_flight(&self) -> usize {
        self.cfg.max_in_flight - self.sem.available_permits()
    }

    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    pub async fn admit(&self) -> Admit {
        if let Ok(p) = self.sem.clone().try_acquire_owned() {
            return Admit::Granted { _permit: p };
        }
        // Reserve the queue slot before awaiting, so the depth check counts
        // waiters rather than lagging behind them.
        let depth = self.queued.fetch_add(1, Ordering::AcqRel) + 1;
        let reservation = QueueReservation(&self.queued);
        if depth > self.cfg.max_queued {
            self.metrics.requests_shed.fetch_add(1, Ordering::Relaxed);
            return Admit::Shed;
        }
        let t0 = Instant::now();
        let acquired = self.sem.clone().acquire_owned().await;
        drop(reservation);
        match acquired {
            Ok(permit) => {
                self.metrics.admission_wait.record(t0.elapsed());
                Admit::Granted { _permit: permit }
            }
            // Nothing closes the semaphore; if it were closed, refusing is
            // the only answer that does not overcommit the cells.
            Err(_) => {
                self.metrics.requests_shed.fetch_add(1, Ordering::Relaxed);
                Admit::Shed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn grants_up_to_the_in_flight_cap_without_queueing() {
        let ac = AdmissionControl::new(
            ShedConfig {
                max_in_flight: 3,
                max_queued: 1,
            },
            Arc::new(ProxyMetrics::new()),
        );
        let mut held = Vec::new();
        for _ in 0..3 {
            match ac.admit().await {
                Admit::Granted { _permit } => held.push(_permit),
                Admit::Shed => panic!("a request within the in-flight cap must be granted"),
            }
        }
        assert_eq!(ac.queued(), 0, "nothing queued while slots were free");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sheds_once_the_queue_is_deeper_than_configured() {
        let metrics = Arc::new(ProxyMetrics::new());
        let ac = Arc::new(AdmissionControl::new(
            ShedConfig {
                max_in_flight: 1,
                max_queued: 2,
            },
            metrics.clone(),
        ));

        let held = match ac.admit().await {
            Admit::Granted { _permit } => _permit,
            Admit::Shed => panic!("the first admit must be granted"),
        };

        let waiters: Vec<_> = (0..2)
            .map(|_| {
                let ac = ac.clone();
                tokio::spawn(async move { matches!(ac.admit().await, Admit::Granted { .. }) })
            })
            .collect();

        while ac.queued() < 2 {
            tokio::task::yield_now().await;
        }

        assert!(
            matches!(ac.admit().await, Admit::Shed),
            "a caller beyond the queue depth is refused at the edge"
        );
        assert_eq!(
            metrics.requests_shed.load(Ordering::Relaxed),
            1,
            "the shed is counted"
        );

        drop(held);
        for w in waiters {
            assert!(
                w.await.expect("waiter task"),
                "a queued caller is granted once the slot frees"
            );
        }
    }

    #[tokio::test]
    async fn cancelling_a_queued_waiter_releases_admission_for_its_replacement() {
        let metrics = Arc::new(ProxyMetrics::new());
        let ac = AdmissionControl::new(
            ShedConfig {
                max_in_flight: 1,
                max_queued: 1,
            },
            metrics.clone(),
        );
        let held = ac.admit().await;
        assert!(
            matches!(&held, Admit::Granted { .. }),
            "the first caller must hold the only in-flight slot"
        );
        let mut abandoned = Box::pin(ac.admit());
        assert!(
            futures::poll!(&mut abandoned).is_pending(),
            "the held permit must block the queued caller"
        );
        assert_eq!(
            ac.queued(),
            1,
            "the blocked caller must own one queue reservation"
        );
        drop(abandoned);
        assert_eq!(
            ac.queued(),
            0,
            "canceling a blocked caller must release its queue reservation"
        );
        assert_eq!(
            ac.in_flight(),
            1,
            "canceling a waiter must not release another caller's permit"
        );
        let mut replacement = Box::pin(ac.admit());
        assert!(
            futures::poll!(&mut replacement).is_pending(),
            "a replacement must queue rather than be shed by abandoned ownership"
        );
        drop(held);
        assert!(
            matches!(
                futures::poll!(&mut replacement),
                std::task::Poll::Ready(Admit::Granted { .. })
            ),
            "the replacement must acquire the released permit"
        );
        assert_eq!(
            ac.queued(),
            0,
            "a granted caller must release its queue reservation once"
        );
        assert_eq!(
            metrics.requests_shed.load(Ordering::Relaxed),
            0,
            "canceled waiters must not cause false overload shedding"
        );
    }

    #[tokio::test]
    async fn releasing_a_permit_lets_the_queue_drain() {
        let ac = Arc::new(AdmissionControl::new(
            ShedConfig {
                max_in_flight: 1,
                max_queued: 8,
            },
            Arc::new(ProxyMetrics::new()),
        ));
        let p = match ac.admit().await {
            Admit::Granted { _permit } => _permit,
            Admit::Shed => panic!("the first admit must be granted"),
        };
        assert_eq!(ac.in_flight(), 1, "the permit is counted in flight");
        drop(p);
        assert_eq!(ac.in_flight(), 0, "dropping the permit frees the slot");
        assert!(
            matches!(ac.admit().await, Admit::Granted { .. }),
            "the freed slot is granted again"
        );
    }
}
