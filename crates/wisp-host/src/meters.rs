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
    budget_kills: AtomicU64,
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
    pub fn record_budget_kill(&self) {
        self.budget_kills.fetch_add(1, Ordering::Relaxed);
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
        }
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
