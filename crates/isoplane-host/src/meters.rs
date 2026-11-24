use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_engine::DispatchOutcome;

pub use crate::occupancy_clock::{try_read_mono_ns, OccupancyInterval};

/// Cumulative occupied-worker time, used for limit enforcement and attributed
/// to the tenant.
#[derive(Debug, Default)]
pub struct Meters {
    tenant: Option<Arc<crate::metrics::TenantMetrics>>,
    /// The sole compute meter.
    occupancy_ns: AtomicU64,
    #[cfg(any(test, feature = "test-util"))]
    dispatches: AtomicU64,
    #[cfg(any(test, feature = "test-util"))]
    budget_exits: AtomicU64,
    #[cfg(any(test, feature = "test-util"))]
    instance_holds: AtomicU64,
    #[cfg(any(test, feature = "test-util"))]
    activations_for_holds: AtomicU64,
    #[cfg(any(test, feature = "test-util"))]
    budget_kills: AtomicU64,
    /// occupancy burned past the budget on kills of either phase. Already inside
    /// `occupancy_ns`; counted again only so the share is reportable.
    #[cfg(any(test, feature = "test-util"))]
    overage_ns: AtomicU64,
}

impl Meters {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn for_tenant(tenant: Arc<crate::metrics::TenantMetrics>) -> Self {
        Self {
            tenant: Some(tenant),
            ..Self::default()
        }
    }

    fn record_measured_occupancy(&self, ns: u64) {
        self.occupancy_ns.fetch_add(ns, Ordering::Relaxed);
        if let Some(tenant) = &self.tenant {
            tenant.record_occupancy(ns);
        }
    }

    pub fn record_occupancy(&self, out: &DispatchOutcome) {
        self.record_measured_occupancy(out.occupancy_ns);
    }

    /// `None`, an unreadable clock, records no occupied time.
    pub fn record_interval(&self, ns: Option<u64>) {
        if let Some(ns) = ns {
            self.record_measured_occupancy(ns);
        }
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn record_dispatch(&self) {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(any(test, feature = "test-util"))]
    pub fn record_grace_end(&self, phase: isoplane_engine::GraceEnd, overage_ns: u64) {
        match phase {
            isoplane_engine::GraceEnd::Exited => &self.budget_exits,
            isoplane_engine::GraceEnd::Killed => &self.budget_kills,
        }
        .fetch_add(1, Ordering::Relaxed);
        self.overage_ns.fetch_add(overage_ns, Ordering::Relaxed);
    }
    /// One of the module's instances stopped taking new dispatches while a
    /// dispatch on it was in the grace after its cancellation.
    #[cfg(any(test, feature = "test-util"))]
    pub fn record_instance_hold(&self) {
        self.instance_holds.fetch_add(1, Ordering::Relaxed);
    }
    /// An activation made because the module's instances with room were held.
    #[cfg(any(test, feature = "test-util"))]
    pub fn record_activation_for_hold(&self) {
        self.activations_for_holds.fetch_add(1, Ordering::Relaxed);
    }

    pub fn occupancy_ns(&self) -> u64 {
        self.occupancy_ns.load(Ordering::Relaxed)
    }
    #[cfg(any(test, feature = "test-util"))]
    pub fn dispatches(&self) -> u64 {
        self.dispatches.load(Ordering::Relaxed)
    }
    #[cfg(any(test, feature = "test-util"))]
    pub fn instance_holds(&self) -> u64 {
        self.instance_holds.load(Ordering::Relaxed)
    }
    #[cfg(any(test, feature = "test-util"))]
    pub fn activations_for_holds(&self) -> u64 {
        self.activations_for_holds.load(Ordering::Relaxed)
    }
    #[cfg(any(test, feature = "test-util"))]
    pub fn budget_exits(&self) -> u64 {
        self.budget_exits.load(Ordering::Relaxed)
    }
    #[cfg(any(test, feature = "test-util"))]
    pub fn budget_kills(&self) -> u64 {
        self.budget_kills.load(Ordering::Relaxed)
    }
    /// A subset of [`Meters::occupancy_ns`], never an addition to it.
    #[cfg(any(test, feature = "test-util"))]
    pub fn overage_ns(&self) -> u64 {
        self.overage_ns.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use isoplane_abi::status::PollStatus;
    use isoplane_engine::Completion;

    fn outcome(occupancy_ns: u64, wall_ns: u64) -> DispatchOutcome {
        DispatchOutcome {
            status: PollStatus::Pending,
            completion: Completion::Parked,
            occupancy_ns,
            occupancy_measurement: isoplane_engine::OccupancyMeasurement::Complete,
            execution_limit: None,
            wall_ns,
            trap: None,
            grace_end: None,
            overage_ns: 0,
        }
    }

    #[test]
    fn a_budget_exit_records_the_overage_rather_than_discarding_it() {
        let m = Meters::new();
        let out = DispatchOutcome {
            status: PollStatus::Done { response: 0 },
            completion: Completion::BudgetExit,
            occupancy_ns: 53_000_000,
            occupancy_measurement: isoplane_engine::OccupancyMeasurement::Complete,
            execution_limit: None,
            wall_ns: 53_500_000,
            trap: None,
            grace_end: Some(isoplane_engine::GraceEnd::Exited),
            overage_ns: 3_000_000,
        };
        m.record_occupancy(&out);
        m.record_grace_end(isoplane_engine::GraceEnd::Exited, out.overage_ns);

        assert_eq!(m.occupancy_ns(), 53_000_000, "the meter must not stop at B");
        assert_eq!(m.overage_ns(), 3_000_000, "the overage share is visible");
        assert_eq!(m.budget_exits(), 1, "an exit is counted as one");
        assert_eq!(m.budget_kills(), 0, "and not as a kill");
    }

    #[test]
    fn overage_is_not_silently_forgiven() {
        let m = Meters::new();
        let budget_ns = 50_000_000u64;
        for over in [1_000_000u64, 2_500_000, 4_900_000] {
            let out = DispatchOutcome {
                status: PollStatus::Done { response: 0 },
                completion: Completion::BudgetExit,
                occupancy_ns: budget_ns + over,
                occupancy_measurement: isoplane_engine::OccupancyMeasurement::Complete,
                execution_limit: None,
                wall_ns: 0,
                trap: None,
                grace_end: Some(isoplane_engine::GraceEnd::Exited),
                overage_ns: over,
            };
            m.record_occupancy(&out);
            m.record_grace_end(isoplane_engine::GraceEnd::Exited, out.overage_ns);
        }
        assert_eq!(m.overage_ns(), 8_400_000, "every overage is summed");
        assert!(
            m.occupancy_ns() > 3 * budget_ns,
            "three over-budget dispatches used {} ns, which is at most 3xB; the overage was \
             dropped somewhere",
            m.occupancy_ns()
        );
        assert!(
            m.overage_ns() < m.occupancy_ns(),
            "the overage is a subset of the one compute meter, not an addition to it"
        );
    }

    #[test]
    fn wall_time_is_not_metered_as_occupancy() {
        let m = Meters::new();
        m.record_occupancy(&outcome(1_000, 1_000_000_000));
        assert_eq!(
            m.occupancy_ns(),
            1_000,
            "only the occupancy interval is recorded"
        );
    }
}
