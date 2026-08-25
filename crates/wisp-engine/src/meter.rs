//! Calling-thread CPU time used to measure active guest execution.

use std::sync::atomic::{AtomicU64, Ordering};

/// Failed clock reads across this process.
static CLOCK_READ_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Number of failed thread CPU clock reads across this process.
pub fn clock_read_failures() -> u64 {
    CLOCK_READ_FAILURES.load(Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// Test-only fault injection. Thread-local on purpose: these tests share a
    /// process with every other engine test, and a global switch would turn
    /// one test's injected fault into another test's flake.
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

/// CPU consumed by the calling thread, or `None` if the clock cannot be read.
#[inline]
pub fn try_thread_cpu_nanos() -> Option<u64> {
    if clock_failure_forced() {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a live, writable, correctly-typed timespec and
    // CLOCK_THREAD_CPUTIME_ID is always a valid clock id on Linux.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        CLOCK_READ_FAILURES.fetch_add(1, Ordering::Relaxed);
        // Kept as well as the counter, not instead of it: the assertion is the
        // fast signal on a laptop, the counter is the only signal in release.
        debug_assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
        return None;
    }
    Some((ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64))
}

/// [`try_thread_cpu_nanos`] with the unreadable case flattened to `0`.
///
/// Kept for the budget-check callers, which need a bare `u64` and have no
/// error channel — but the `0` it returns on failure is a **floor, not a
/// measurement**, and the failure is on the record in [`clock_read_failures`]
/// so the two remain separable after the fact. Accounting sites should call the
/// `try_` form.
#[inline]
pub fn thread_cpu_nanos() -> u64 {
    try_thread_cpu_nanos().unwrap_or(0)
}

/// Resolution of the meter, for reporting. Not used on the hot path.
pub fn thread_cpu_resolution_nanos() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: as above.
    unsafe { libc::clock_getres(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_clock_is_counted_rather_than_read_as_zero() {
        // Give this thread a CPU history to misattribute. Without it the thread
        // has barely run and "the whole thread lifetime" is a few hundred
        // microseconds, which understates the bug and makes the assertion
        // below depend on how much work the test harness happened to do.
        let mut x = 0u64;
        for i in 0..8_000_000u64 {
            x = x.wrapping_add(i ^ x);
        }
        std::hint::black_box(x);
        let lifetime = try_thread_cpu_nanos().expect("the clock works at the start of the test");
        assert!(lifetime > 5_000_000, "the warm-up burned only {lifetime} ns");

        let before = clock_read_failures();
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));

        assert_eq!(try_thread_cpu_nanos(), None, "an unreadable clock must say so");
        assert!(clock_read_failures() > before, "the failed read was not counted");

        // The lossy wrapper still answers `0` for the budget-check callers
        // that need a bare `u64` — but it counts, so `0` and "unreadable" stay
        // separable after the fact instead of being fused into one number.
        let mid = clock_read_failures();
        assert_eq!(thread_cpu_nanos(), 0);
        assert_eq!(clock_read_failures(), mid + 1, "the wrapper swallowed the failure");

        // Compare interval calculations using the same failed start reading.
        let start_now = try_thread_cpu_nanos(); // the fixed form: None
        let start_before = thread_cpu_nanos(); // the old form: 0
        FORCE_CLOCK_FAILURE.with(|f| f.set(false));
        let end = try_thread_cpu_nanos().expect("the clock is working again");

        let measured_now = match (start_now, Some(end)) {
            (Some(a), Some(b)) => b.saturating_sub(a),
            _ => 0,
        };
        let measured_before = end.saturating_sub(start_before);

        assert_eq!(
            measured_now, 0,
            "a dispatch whose start read failed recorded {measured_now} ns — a floor of 0 is the \
             only honest answer when the interval was never measured"
        );
        assert!(
            measured_before >= lifetime,
            "the unchecked expression produced {measured_before} ns after {lifetime} ns before dispatch"
        );
        assert!(measured_before > measured_now);

        // And with the clock working the same expression measures normally, so
        // the counter above is a fault signal and not a constant.
        let start = try_thread_cpu_nanos();
        let mut x = 0u64;
        for i in 0..300_000u64 {
            x = x.wrapping_add(i ^ x);
        }
        std::hint::black_box(x);
        let quiet = clock_read_failures();
        let measured = match (start, try_thread_cpu_nanos()) {
            (Some(a), Some(b)) => b.saturating_sub(a),
            _ => 0,
        };
        assert!(measured > 0, "a working clock measured nothing");
        assert_eq!(quiet, clock_read_failures(), "a working read was counted as a failure");
    }

    /// The injection is thread-local. A global switch would make every CPU
    /// measurement in this test binary — including `the_clock_measures_work_separately_from_sleep`
    /// below — suspect.
    #[test]
    fn the_clock_fault_injection_does_not_escape_its_thread() {
        FORCE_CLOCK_FAILURE.with(|f| f.set(true));
        assert_eq!(try_thread_cpu_nanos(), None);
        let elsewhere = std::thread::spawn(try_thread_cpu_nanos).join().unwrap();
        assert!(elsewhere.is_some(), "the injected fault leaked into another thread");
        FORCE_CLOCK_FAILURE.with(|f| f.set(false));
    }

    #[test]
    fn the_clock_measures_work_separately_from_sleep() {
        let a = thread_cpu_nanos();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let slept = thread_cpu_nanos() - a;

        let b = thread_cpu_nanos();
        let mut x = 0u64;
        let t = std::time::Instant::now();
        while t.elapsed() < std::time::Duration::from_millis(50) {
            for i in 0..4096u64 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
            }
        }
        std::hint::black_box(x);
        let burned = thread_cpu_nanos() - b;

        // Compare sleeping and spinning on the same thread CPU clock.
        assert!(slept < 5_000_000, "sleeping recorded {slept} ns of CPU");
        assert!(burned > 40_000_000, "50 ms of spinning recorded only {burned} ns");
    }
}
