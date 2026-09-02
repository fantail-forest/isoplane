//! Admission control: the proxy sheds before cells saturate.
//!
//! Two knobs, and the second is the interesting one:
//!
//! * `max_in_flight` — how many dispatches may be Hot in the cells at once.
//! * `max_queued` — how many callers may *wait* for a slot before the proxy
//!   starts refusing.
//!
//! Without the second knob a proxy under overload converts a CPU shortage into
//! an unbounded queue, and every request then times out having consumed a
//! thread and a socket. Shedding at the edge is what keeps the failure mode
//! "some requests get a fast 503" rather than "all requests get a slow 504" —
//! and it is the proxy's job because cells queue at admission, and the shed
//! has to happen before that queue exists.

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
    Granted(#[allow(dead_code)] OwnedSemaphorePermit),
    /// Refused at the edge. The caller should answer 503 with `Retry-After`.
    Shed,
}

pub struct AdmissionControl {
    sem: Arc<Semaphore>,
    queued: AtomicUsize,
    cfg: ShedConfig,
    metrics: Arc<ProxyMetrics>,
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
        // Fast path: a free slot means no queue at all.
        if let Ok(p) = self.sem.clone().try_acquire_owned() {
            return Admit::Granted(p);
        }
        // Reserve a queue slot *before* awaiting, so the depth check reflects
        // waiters rather than lagging behind them.
        let depth = self.queued.fetch_add(1, Ordering::AcqRel) + 1;
        if depth > self.cfg.max_queued {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            self.metrics.requests_shed.fetch_add(1, Ordering::Relaxed);
            return Admit::Shed;
        }
        let t0 = Instant::now();
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .expect("admission semaphore is never closed");
        self.queued.fetch_sub(1, Ordering::AcqRel);
        self.metrics.admission_wait.record(t0.elapsed());
        Admit::Granted(permit)
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
            assert!(matches!(ac.admit().await, Admit::Granted(_)));
            held.push(());
        }
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

        // Occupy the only slot.
        let held = match ac.admit().await {
            Admit::Granted(p) => p,
            Admit::Shed => panic!("first admit must be granted"),
        };

        // Two waiters fit in the queue.
        let waiters: Vec<_> = (0..2)
            .map(|_| {
                let ac = ac.clone();
                tokio::spawn(async move { matches!(ac.admit().await, Admit::Granted(_)) })
            })
            .collect();

        // Give the waiters time to actually enqueue.
        while ac.queued() < 2 {
            tokio::task::yield_now().await;
        }

        // The third is refused at the edge rather than queued behind them.
        assert!(matches!(ac.admit().await, Admit::Shed));
        assert_eq!(metrics.requests_shed.load(Ordering::Relaxed), 1);

        drop(held);
        for w in waiters {
            assert!(w.await.unwrap());
        }
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
            Admit::Granted(p) => p,
            _ => unreachable!(),
        };
        assert_eq!(ac.in_flight(), 1);
        drop(p);
        assert_eq!(ac.in_flight(), 0);
        assert!(matches!(ac.admit().await, Admit::Granted(_)));
    }
}
