//! Per-tenant worker occupancy, measured during execution and attributed host
//! work. Parked time is recorded separately from occupied-worker time.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_engine::DispatchOutcome;

pub use crate::occupancy_clock::{
    clock_read_failures, read_occupancy_ns, try_read_mono_ns, try_read_occupancy_ns,
    OccupancyInterval,
};

/// Cumulative execution counters used for limit enforcement and diagnostics.
#[derive(Debug, Default)]
pub struct Meters {
    tenant: Option<Arc<crate::metrics::TenantMetrics>>,
    /// The sole compute meter.
    occupancy_ns: AtomicU64,
    /// Telemetry only; excluded from occupied-worker time.
    wall_ns: AtomicU64,
    dispatches: AtomicU64,
    hibernation_wakes: AtomicU64,
    /// Snapshot bytes captured, for storage diagnostics.
    delta_bytes: AtomicU64,
    captures: AtomicU64,
    /// Telemetry only; excluded from occupied-worker time.
    observed_park_ms: AtomicU64,
    /// Already inside `occupancy_ns`; broken out so the share is visible.
    log_occupancy_ns: AtomicU64,
    trapped: AtomicU64,
    budget_exits: AtomicU64,
    instance_holds: AtomicU64,
    instance_hold_ms: AtomicU64,
    activations_for_holds: AtomicU64,
    memory_limit_retirements: AtomicU64,
    budget_kills: AtomicU64,
    /// occupancy burned past the budget on kills of either phase. Already inside
    /// `occupancy_ns`; counted again only so the share is reportable, and never
    /// discarded, so over-budget execution remains visible.
    overage_ns: AtomicU64,
    shed: AtomicU64,
    /// Slices and reactor polls whose monotonic clock could not be read at one end and
    /// which are marked incomplete.
    incomplete_intervals: AtomicU64,
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

    pub(crate) fn record_completion(&self, trapped: bool) {
        if let Some(tenant) = &self.tenant {
            tenant.record_completion(trapped);
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
        if out.occupancy_measurement == isoplane_engine::OccupancyMeasurement::Incomplete {
            self.incomplete_intervals.fetch_add(1, Ordering::Relaxed);
        }
        self.wall_ns.fetch_add(out.wall_ns, Ordering::Relaxed);
    }

    /// `None`, an unreadable clock, records no occupied time and counts the gap.
    pub fn record_interval(&self, ns: Option<u64>) {
        match ns {
            Some(ns) => {
                self.record_measured_occupancy(ns);
            }
            None => {
                self.incomplete_intervals.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn record_dispatch(&self) {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_hibernation_wake(&self) {
        self.hibernation_wakes.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_capture(&self, delta_bytes: u64) {
        self.captures.fetch_add(1, Ordering::Relaxed);
        self.delta_bytes.fetch_add(delta_bytes, Ordering::Relaxed);
    }
    pub fn record_park(&self, ms: u64) {
        self.observed_park_ms.fetch_add(ms, Ordering::Relaxed);
    }
    pub fn record_log_occupancy(&self, ns: u64) {
        self.log_occupancy_ns.fetch_add(ns, Ordering::Relaxed);
    }
    pub fn record_trap(&self) {
        self.trapped.fetch_add(1, Ordering::Relaxed);
    }
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
    pub fn record_instance_hold(&self) {
        self.instance_holds.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_instance_hold_ms(&self, ms: u64) {
        self.instance_hold_ms.fetch_add(ms, Ordering::Relaxed);
    }
    /// An activation made because the module's instances with room were held.
    pub fn record_activation_for_hold(&self) {
        self.activations_for_holds.fetch_add(1, Ordering::Relaxed);
    }
    /// One of the module's instances passed its memory limit and was retired.
    pub fn record_memory_limit_retirement(&self) {
        self.memory_limit_retirements
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_shed(&self) {
        self.shed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn occupancy_ns(&self) -> u64 {
        self.occupancy_ns.load(Ordering::Relaxed)
    }
    pub fn occupancy_ms(&self) -> f64 {
        self.occupancy_ns() as f64 / 1e6
    }
    pub fn wall_ns(&self) -> u64 {
        self.wall_ns.load(Ordering::Relaxed)
    }
    pub fn dispatches(&self) -> u64 {
        self.dispatches.load(Ordering::Relaxed)
    }
    pub fn hibernation_wakes(&self) -> u64 {
        self.hibernation_wakes.load(Ordering::Relaxed)
    }
    pub fn delta_bytes(&self) -> u64 {
        self.delta_bytes.load(Ordering::Relaxed)
    }
    pub fn captures(&self) -> u64 {
        self.captures.load(Ordering::Relaxed)
    }
    pub fn observed_park_ms(&self) -> u64 {
        self.observed_park_ms.load(Ordering::Relaxed)
    }
    pub fn log_occupancy_ns(&self) -> u64 {
        self.log_occupancy_ns.load(Ordering::Relaxed)
    }
    pub fn trapped(&self) -> u64 {
        self.trapped.load(Ordering::Relaxed)
    }
    pub fn instance_holds(&self) -> u64 {
        self.instance_holds.load(Ordering::Relaxed)
    }
    pub fn instance_hold_ms(&self) -> u64 {
        self.instance_hold_ms.load(Ordering::Relaxed)
    }
    pub fn memory_limit_retirements(&self) -> u64 {
        self.memory_limit_retirements.load(Ordering::Relaxed)
    }
    pub fn activations_for_holds(&self) -> u64 {
        self.activations_for_holds.load(Ordering::Relaxed)
    }
    pub fn budget_exits(&self) -> u64 {
        self.budget_exits.load(Ordering::Relaxed)
    }
    pub fn budget_kills(&self) -> u64 {
        self.budget_kills.load(Ordering::Relaxed)
    }
    /// A subset of [`Meters::occupancy_ns`], never an addition to it.
    pub fn overage_ns(&self) -> u64 {
        self.overage_ns.load(Ordering::Relaxed)
    }
    pub fn shed(&self) -> u64 {
        self.shed.load(Ordering::Relaxed)
    }
    pub fn incomplete_intervals(&self) -> u64 {
        self.incomplete_intervals.load(Ordering::Relaxed)
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
            grace_deferrals: 0,
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
            grace_deferrals: 0,
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
                grace_deferrals: 0,
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
    fn wall_clock_is_recorded_separately_from_occupancy() {
        let m = Meters::new();
        m.record_occupancy(&outcome(1_000, 1_000_000_000));
        m.record_park(60_000);
        assert_eq!(
            m.occupancy_ns(),
            1_000,
            "only the occupancy interval is recorded"
        );
        assert_eq!(m.observed_park_ms(), 60_000, "park time is recorded");
        assert_eq!(
            m.occupancy_ms(),
            0.001,
            "a second parked adds no occupied time"
        );
    }
}
