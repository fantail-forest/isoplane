//! Per-application CPU usage and dispatch counters.
//! Wall-clock and park time are recorded separately.

use std::sync::atomic::{AtomicU64, Ordering};

use isoplane_engine::DispatchOutcome;

pub use crate::cpu_clock::{
    clock_read_failures, read_thread_cpu_ns, try_read_mono_ns, try_read_thread_cpu_ns, CpuInterval,
};

/// Dispatch and lifecycle counters for one registered module.
#[derive(Debug, Default)]
pub struct Meters {
    /// Accumulated thread CPU time.
    cpu_ns: AtomicU64,
    /// Recorded separately from active execution time.
    wall_ns: AtomicU64,
    dispatches: AtomicU64,
    hibernation_wakes: AtomicU64,
    /// Accumulated captured delta bytes.
    delta_bytes: AtomicU64,
    captures: AtomicU64,
    /// Recorded separately from active execution time.
    observed_park_ms: AtomicU64,
    /// Already inside `cpu_ns`; broken out so the share is visible.
    log_cpu_ns: AtomicU64,
    trapped: AtomicU64,
    budget_kills: AtomicU64,
    clean_kills: AtomicU64,
    hard_kills: AtomicU64,
    /// CPU consumed past the budget, already included in `cpu_ns`.
    overage_ns: AtomicU64,
    shed: AtomicU64,
    /// Active intervals with an unreadable thread clock endpoint.
    incomplete_intervals: AtomicU64,
}

impl Meters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_cpu(&self, out: &DispatchOutcome) {
        self.cpu_ns.fetch_add(out.cpu_ns, Ordering::Relaxed);
        if out.cpu_measurement == isoplane_engine::CpuMeasurement::Incomplete {
            self.incomplete_intervals.fetch_add(1, Ordering::Relaxed);
        }
        self.wall_ns.fetch_add(out.wall_ns, Ordering::Relaxed);
    }

    /// Record active time, counting an unreadable interval separately.
    pub fn record_interval(&self, ns: Option<u64>) {
        match ns {
            Some(ns) => {
                self.cpu_ns.fetch_add(ns, Ordering::Relaxed);
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
    pub fn record_log_cpu(&self, ns: u64) {
        self.log_cpu_ns.fetch_add(ns, Ordering::Relaxed);
    }
    pub fn record_trap(&self) {
        self.trapped.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_budget_kill(&self, phase: isoplane_engine::KillPhase, overage_ns: u64) {
        self.budget_kills.fetch_add(1, Ordering::Relaxed);
        match phase {
            isoplane_engine::KillPhase::Clean => &self.clean_kills,
            isoplane_engine::KillPhase::Hard => &self.hard_kills,
        }
        .fetch_add(1, Ordering::Relaxed);
        self.overage_ns.fetch_add(overage_ns, Ordering::Relaxed);
    }
    pub fn record_shed(&self) {
        self.shed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn cpu_ns(&self) -> u64 {
        self.cpu_ns.load(Ordering::Relaxed)
    }
    pub fn cpu_ms(&self) -> f64 {
        self.cpu_ns() as f64 / 1e6
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
    pub fn log_cpu_ns(&self) -> u64 {
        self.log_cpu_ns.load(Ordering::Relaxed)
    }
    pub fn trapped(&self) -> u64 {
        self.trapped.load(Ordering::Relaxed)
    }
    pub fn budget_kills(&self) -> u64 {
        self.budget_kills.load(Ordering::Relaxed)
    }
    pub fn clean_kills(&self) -> u64 {
        self.clean_kills.load(Ordering::Relaxed)
    }
    pub fn hard_kills(&self) -> u64 {
        self.hard_kills.load(Ordering::Relaxed)
    }
    /// A subset of [`Meters::cpu_ns`], never an addition to it.
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

    fn outcome(cpu_ns: u64, wall_ns: u64) -> DispatchOutcome {
        DispatchOutcome {
            status: PollStatus::Pending,
            completion: Completion::Parked,
            cpu_ns,
            cpu_measurement: isoplane_engine::CpuMeasurement::Complete,
            execution_limit: None,
            wall_ns,
            trap: None,
            kill: None,
            overage_ns: 0,
            grace_deferrals: 0,
        }
    }

    #[test]
    fn a_clean_kill_records_the_overage_rather_than_discarding_it() {
        let m = Meters::new();
        let out = DispatchOutcome {
            status: PollStatus::Done { response: 0 },
            completion: Completion::CleanKill,
            cpu_ns: 53_000_000,
            cpu_measurement: isoplane_engine::CpuMeasurement::Complete,
            execution_limit: None,
            wall_ns: 53_500_000,
            trap: None,
            kill: Some(isoplane_engine::KillPhase::Clean),
            overage_ns: 3_000_000,
            grace_deferrals: 0,
        };
        m.record_cpu(&out);
        m.record_budget_kill(isoplane_engine::KillPhase::Clean, out.overage_ns);

        assert_eq!(m.cpu_ns(), 53_000_000, "the meter must not stop at B");
        assert_eq!(m.overage_ns(), 3_000_000, "the overage share is visible");
        assert_eq!(m.clean_kills(), 1, "a clean kill is counted as one");
        assert_eq!(m.hard_kills(), 0, "and not as a hard kill");
        assert_eq!(m.budget_kills(), 1, "both phases count as budget kills");
    }

    #[test]
    fn overage_is_not_silently_forgiven() {
        let m = Meters::new();
        let budget_ns = 50_000_000u64;
        for over in [1_000_000u64, 2_500_000, 4_900_000] {
            let out = DispatchOutcome {
                status: PollStatus::Done { response: 0 },
                completion: Completion::CleanKill,
                cpu_ns: budget_ns + over,
                cpu_measurement: isoplane_engine::CpuMeasurement::Complete,
                execution_limit: None,
                wall_ns: 0,
                trap: None,
                kill: Some(isoplane_engine::KillPhase::Clean),
                overage_ns: over,
                grace_deferrals: 0,
            };
            m.record_cpu(&out);
            m.record_budget_kill(isoplane_engine::KillPhase::Clean, out.overage_ns);
        }
        assert_eq!(m.overage_ns(), 8_400_000, "every overage is summed");
        assert!(
            m.cpu_ns() > 3 * budget_ns,
            "three over-budget dispatches measured {} ns, which is at most 3xB; the overage was \
             dropped somewhere",
            m.cpu_ns()
        );
        assert!(
            m.overage_ns() < m.cpu_ns(),
            "the overage is a subset of the one compute meter, not an addition to it"
        );
    }

    #[test]
    fn wall_clock_is_recorded_separately_from_active_time() {
        let m = Meters::new();
        m.record_cpu(&outcome(1_000, 1_000_000_000));
        m.record_park(60_000);
        assert_eq!(m.cpu_ns(), 1_000, "only the CPU interval is measured");
        assert_eq!(m.observed_park_ms(), 60_000, "park time is recorded");
        assert_eq!(m.cpu_ms(), 0.001, "a second off-CPU adds no CPU time");
    }
}
