//! Per-tenant worker occupancy, measured during execution and attributed host
//! work.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use isoplane_engine::DispatchOutcome;

pub use crate::occupancy_clock::{clock_read_failures, try_read_mono_ns, OccupancyInterval};

/// Cumulative occupied-worker time, used for limit enforcement and attributed
/// to the tenant.
#[derive(Debug, Default)]
pub struct Meters {
    tenant: Option<Arc<crate::metrics::TenantMetrics>>,
    /// The sole compute meter.
    occupancy_ns: AtomicU64,
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
    }

    /// `None`, an unreadable clock, records no occupied time.
    pub fn record_interval(&self, ns: Option<u64>) {
        if let Some(ns) = ns {
            self.record_measured_occupancy(ns);
        }
    }

    pub fn occupancy_ns(&self) -> u64 {
        self.occupancy_ns.load(Ordering::Relaxed)
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
