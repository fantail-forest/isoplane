//! Application instance reservations, including activations and draining instances.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(crate) struct InstanceLimit(pub(crate) u32);

#[derive(Default)]
pub(crate) struct ApplicationInstances(AtomicU32);

#[derive(Debug, thiserror::Error)]
#[error("application instance limit {limit} reached")]
pub(crate) struct InstanceLimitReached {
    limit: u32,
}

pub(crate) struct InstanceReservation {
    application: Arc<ApplicationInstances>,
}

impl ApplicationInstances {
    pub(crate) fn reserve(
        self: &Arc<Self>,
        limit: InstanceLimit,
    ) -> Result<InstanceReservation, InstanceLimitReached> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < limit.0).then(|| count + 1)
            })
            .map(|_| InstanceReservation {
                application: self.clone(),
            })
            .map_err(|_| InstanceLimitReached { limit: limit.0 })
    }
}

impl Drop for InstanceReservation {
    fn drop(&mut self) {
        self.application.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_activations_reserve_capacity_before_starting() {
        let instances = Arc::new(ApplicationInstances::default());
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let tasks: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        let reservation = instances.reserve(InstanceLimit(3));
                        barrier.wait();
                        let accepted = reservation.is_ok();
                        barrier.wait();
                        accepted
                    })
                })
                .collect();
            assert_eq!(
                tasks
                    .into_iter()
                    .map(|task| task.join().expect("admission thread"))
                    .filter(|accepted| *accepted)
                    .count(),
                3,
            );
        });
        assert_eq!(instances.0.load(Ordering::Acquire), 0);
        assert!(instances.reserve(InstanceLimit(1)).is_ok());
    }
}
