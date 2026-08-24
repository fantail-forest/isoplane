//! Per-application CPU usage and dispatch counters.
//! Wall-clock and park time are recorded separately.

use std::sync::atomic::{AtomicU64, Ordering};

use wisp_engine::DispatchOutcome;

/// Calling-thread CPU time, or `0` if the clock cannot be read.
#[allow(unsafe_code)]
pub fn read_thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, properly aligned `timespec` we own for the
    // duration of the call; `clock_gettime` writes it and returns 0 or -1.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

/// Process CPU time, summed across all threads.
#[allow(unsafe_code)]
pub fn read_process_cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: as `read_thread_cpu_ns`.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

/// A scoped thread-CPU measurement over an active guest interval.
pub struct CpuInterval {
    start: u64,
}

impl CpuInterval {
    pub fn start() -> Self {
        Self {
            start: read_thread_cpu_ns(),
        }
    }
    pub fn elapsed_ns(&self) -> u64 {
        read_thread_cpu_ns().saturating_sub(self.start)
    }
}

/// Dispatch and lifecycle counters for one registered module.
#[derive(Debug, Default)]
pub struct Meters {
    /// Accumulated thread CPU time.
    cpu_ns: AtomicU64,
    /// Recorded separately from CPU usage.
    wall_ns: AtomicU64,
    /// Raw dispatch count.
    dispatches: AtomicU64,
    /// Wakes delivered from the parked state.
    wakes: AtomicU64,
    /// Hibernation wake count.
    hibernation_wakes: AtomicU64,
    /// Accumulated captured delta bytes.
    delta_bytes: AtomicU64,
    captures: AtomicU64,
    /// Accumulated park duration in milliseconds.
    observed_park_ms: AtomicU64,
    /// Log CPU counts against the dispatch budget, which is what stops
    /// log-as-compute abuse. Already included in `cpu_ns` (it is spent on the
    /// pool thread); broken out so the share is visible.
    log_cpu_ns: AtomicU64,
    trapped: AtomicU64,
    /// Every CPU-budget kill this tenant took, both phases.
    budget_kills: AtomicU64,
    /// Clean and hard CPU-budget kill counts.
    clean_kills: AtomicU64,
    hard_kills: AtomicU64,
    /// CPU consumed past the budget, already included in `cpu_ns`.
    overage_ns: AtomicU64,
    shed: AtomicU64,
}

impl Meters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the dispatch's CPU and wall-clock measurements.
    pub fn record_cpu(&self, out: &DispatchOutcome) {
        self.cpu_ns.fetch_add(out.cpu_ns, Ordering::Relaxed);
        self.wall_ns.fetch_add(out.wall_ns, Ordering::Relaxed);
    }

    /// Record one top-level work item.
    pub fn record_dispatch(&self) {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_wake(&self) {
        self.wakes.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_hibernation_wake(&self) {
        self.hibernation_wakes.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_capture(&self, delta_bytes: u64) {
        self.captures.fetch_add(1, Ordering::Relaxed);
        self.delta_bytes.fetch_add(delta_bytes, Ordering::Relaxed);
    }
    /// Accumulated park duration.
    pub fn record_park(&self, ms: u64) {
        self.observed_park_ms.fetch_add(ms, Ordering::Relaxed);
    }
    pub fn record_log_cpu(&self, ns: u64) {
        self.log_cpu_ns.fetch_add(ns, Ordering::Relaxed);
    }
    pub fn record_trap(&self) {
        self.trapped.fetch_add(1, Ordering::Relaxed);
    }
    /// Record the over-budget share already included in `record_cpu`.
    pub fn record_budget_kill(&self, phase: wisp_engine::KillPhase, overage_ns: u64) {
        self.budget_kills.fetch_add(1, Ordering::Relaxed);
        match phase {
            wisp_engine::KillPhase::Clean => &self.clean_kills,
            wisp_engine::KillPhase::Hard => &self.hard_kills,
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
    pub fn wakes(&self) -> u64 {
        self.wakes.load(Ordering::Relaxed)
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
    /// CPU consumed past the budget, already included in [`Meters::cpu_ns`].
    pub fn overage_ns(&self) -> u64 {
        self.overage_ns.load(Ordering::Relaxed)
    }
    pub fn shed(&self) -> u64 {
        self.shed.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::status::PollStatus;
    use wisp_engine::Completion;

    fn outcome(cpu_ns: u64, wall_ns: u64) -> DispatchOutcome {
        DispatchOutcome {
            status: PollStatus::Pending,
            completion: Completion::Parked,
            cpu_ns,
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
            wall_ns: 53_500_000,
            trap: None,
            kill: Some(wisp_engine::KillPhase::Clean),
            overage_ns: 3_000_000,
            grace_deferrals: 0,
        };
        m.record_cpu(&out);
        m.record_budget_kill(wisp_engine::KillPhase::Clean, out.overage_ns);

        assert_eq!(m.cpu_ns(), 53_000_000, "the meter must not stop at B");
        assert_eq!(m.overage_ns(), 3_000_000);
        assert_eq!(m.clean_kills(), 1);
        assert_eq!(m.hard_kills(), 0);
        assert_eq!(m.budget_kills(), 1);
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
                wall_ns: 0,
                trap: None,
                kill: Some(wisp_engine::KillPhase::Clean),
                overage_ns: over,
                grace_deferrals: 0,
            };
            m.record_cpu(&out);
            m.record_budget_kill(wisp_engine::KillPhase::Clean, out.overage_ns);
        }
        assert_eq!(m.overage_ns(), 8_400_000);
        assert!(
            m.cpu_ns() > 3 * budget_ns,
            "three over-budget dispatches measured {} ns, which is at most 3xB — the overage was \
             dropped somewhere",
            m.cpu_ns()
        );
        // Overage is a subset of the compute total, not an addition.
        assert!(m.overage_ns() < m.cpu_ns());
    }

    #[test]
    fn wall_clock_is_recorded_separately_from_active_time() {
        let m = Meters::new();
        m.record_cpu(&outcome(1_000, 1_000_000_000));
        m.record_park(60_000);
        assert_eq!(m.cpu_ns(), 1_000);
        assert_eq!(m.observed_park_ms(), 60_000);
        assert_eq!(m.cpu_ms(), 0.001);
    }

    #[test]
    fn thread_cpu_clock_advances_only_while_burning() {
        let idle = CpuInterval::start();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let slept = idle.elapsed_ns();

        let busy = CpuInterval::start();
        let mut x = 0u64;
        for i in 0..3_000_000u64 {
            x = x.wrapping_add(i ^ x);
        }
        std::hint::black_box(x);
        let burned = busy.elapsed_ns();

        // 50 ms of sleep must not read as 50 ms of CPU.
        assert!(slept < 5_000_000, "sleeping recorded {slept} ns of CPU");
        assert!(
            burned > slept,
            "burning {burned} ns did not exceed sleeping {slept} ns"
        );
    }
}
