//! Per-application CPU usage and dispatch counters.
//! Wall-clock and park time are recorded separately.

use std::sync::atomic::{AtomicU64, Ordering};

use wisp_engine::DispatchOutcome;

/// Failed clock reads across this process.
static CLOCK_READ_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Failed clock reads across this process.
pub fn clock_read_failures() -> u64 {
    CLOCK_READ_FAILURES.load(Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// Test-only fault injection for the unreadable-clock path. Thread-local
    /// on purpose: the unit tests share a process, and a global switch would
    /// make one test's fault another test's flake.
    static FORCE_CLOCK_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn clock_failure_forced() -> bool {
    FORCE_CLOCK_FAILURE.with(|f| f.get())
}

#[cfg(not(test))]
#[inline(always)]
fn clock_failure_forced() -> bool {
    false
}

#[allow(unsafe_code)]
fn read_clock_ns(clock: libc::clockid_t) -> Option<u64> {
    if clock_failure_forced() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, properly aligned `timespec` we own for the
    // duration of the call; `clock_gettime` writes it and returns 0 or -1.
    let rc = unsafe { libc::clock_gettime(clock, &mut ts) };
    if rc != 0 {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    Some((ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64))
}

/// Calling-thread CPU time, or `None` if the clock could not be read.
pub fn try_read_thread_cpu_ns() -> Option<u64> {
    read_clock_ns(libc::CLOCK_THREAD_CPUTIME_ID)
}

/// `CLOCK_PROCESS_CPUTIME_ID` — every thread in this process summed — or
/// `None` if the clock could not be read.
pub fn try_read_process_cpu_ns() -> Option<u64> {
    read_clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID)
}

/// `CLOCK_MONOTONIC`, **raw**, or `None` if the clock could not be read.
///
/// Raw is the point: no per-process origin is subtracted, so a reading taken
/// here and a reading taken by any other crate in this process — notably
/// `wisp_bench::schedule::mono_ns`, which is this same `clock_gettime` call —
/// are on one epoch and may be subtracted from each other. See
/// [`crate::mono_ns`] for what the origin used to be and why it had to go.
pub fn try_read_mono_ns() -> Option<u64> {
    read_clock_ns(libc::CLOCK_MONOTONIC)
}

/// [`try_read_thread_cpu_ns`] with the unreadable case flattened to `0`.
///
/// Kept because callers outside this module read a bare `u64`, but the zero
/// it returns on failure is a **floor, not a measurement**; the failure is
/// counted in [`clock_read_failures`] so the two are still separable after
/// the fact. New code should call the `try_` form.
pub fn read_thread_cpu_ns() -> u64 {
    try_read_thread_cpu_ns().unwrap_or(0)
}

/// [`try_read_process_cpu_ns`] with an unreadable clock represented as `0`.
pub fn read_process_cpu_ns() -> u64 {
    try_read_process_cpu_ns().unwrap_or(0)
}

/// A scoped thread-CPU measurement over an active guest interval.
pub struct CpuInterval {
    /// The start reading, or `None` when the clock could not be read.
    start: Option<u64>,
}

impl CpuInterval {
    pub fn start() -> Self {
        Self {
            start: try_read_thread_cpu_ns(),
        }
    }
    /// Elapsed thread CPU time, or `None` if either endpoint is unreadable.
    pub fn try_elapsed_ns(&self) -> Option<u64> {
        Some(try_read_thread_cpu_ns()?.saturating_sub(self.start?))
    }
    /// The interval, with an unreadable clock flattened to `0`. The failure is
    /// still visible in [`clock_read_failures`].
    pub fn elapsed_ns(&self) -> u64 {
        self.try_elapsed_ns().unwrap_or(0)
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
    /// Active intervals with an unreadable endpoint clock.
    incomplete_intervals: AtomicU64,
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

    /// Record measured CPU time, counting an unreadable interval separately.
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
    /// Active intervals with an unreadable endpoint clock.
    pub fn incomplete_intervals(&self) -> u64 {
        self.incomplete_intervals.load(Ordering::Relaxed)
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
    fn an_unreadable_clock_is_not_a_zero_measurement() {
        let before = clock_read_failures();
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));

        assert_eq!(
            try_read_thread_cpu_ns(),
            None,
            "an unreadable clock must say so"
        );
        assert_eq!(try_read_process_cpu_ns(), None);
        let failures = clock_read_failures();
        assert!(failures >= before + 2, "the failed reads were not counted");

        // The lossy wrappers still answer `0` for callers that take a bare
        // `u64` — but the failure is on the record, so the two are separable
        // after the fact rather than fused into one number.
        assert_eq!(read_thread_cpu_ns(), 0);
        assert!(clock_read_failures() > failures);

        let iv = CpuInterval::start();
        assert_eq!(
            iv.try_elapsed_ns(),
            None,
            "an interval with an unreadable end is not a number"
        );
        let m = Meters::new();
        m.record_interval(iv.try_elapsed_ns());
        assert_eq!(
            m.cpu_ns(),
            0,
            "an unmeasured interval must not be invented"
        );
        assert_eq!(m.incomplete_intervals(), 1, "...and must not vanish either");

        FORCE_CLOCK_FAILURE.with(|f| f.set(false));

        let iv = CpuInterval::start();
        let mut x = 0u64;
        for i in 0..300_000u64 {
            x = x.wrapping_add(i ^ x);
        }
        std::hint::black_box(x);
        let measured = iv.try_elapsed_ns();
        assert!(
            measured.is_some(),
            "a working clock must produce a measurement"
        );
        m.record_interval(measured);
        assert!(m.cpu_ns() > 0);
        assert_eq!(
            m.incomplete_intervals(),
            1,
            "the working read must not be counted as a fault"
        );
    }

    /// The fault injection is thread-local, so one test's broken clock is not
    /// another test's flake. Worth asserting: a global switch here would make
    /// every CPU measurement in this binary suspect.
    #[test]
    fn the_clock_fault_injection_does_not_escape_its_thread() {
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));
        assert_eq!(try_read_thread_cpu_ns(), None);
        let elsewhere = std::thread::spawn(try_read_thread_cpu_ns).join().unwrap();
        assert!(
            elsewhere.is_some(),
            "the injected fault leaked into another thread"
        );
        FORCE_CLOCK_FAILURE.with(|f| f.set(false));
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
