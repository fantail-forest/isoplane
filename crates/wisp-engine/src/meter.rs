//! The compute meter (spec.md §6.4, §11.2).
//!
//! > *measured CPU = `CLOCK_THREAD_CPUTIME_ID` accumulated over the dispatch's
//! > Hot intervals — exact under preemption, immune to wall-clock noise, and
//! > the sole compute meter (G2).*
//!
//! Because the meter reads the *calling thread's* CPU clock and a dispatch is
//! a synchronous call on one execution-pool thread (§6.1), park time is not
//! "excluded from CPU accounting" by policy — it is structurally unreachable by the
//! meter. A Parked instance holds no thread, so there is no clock to read.

/// Nanoseconds of CPU consumed by the calling thread.
///
/// Validated by the de-risking spike (`crates/spike/src/q4_cputime.rs`): a busy
/// thread and a sleeping thread in the same process, running concurrently, are
/// accounted separately, and the read is a vDSO call rather than a syscall on
/// this platform.
#[inline]
pub fn thread_cpu_nanos() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a live, writable, correctly-typed timespec and
    // CLOCK_THREAD_CPUTIME_ID is always a valid clock id on Linux.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    debug_assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
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
    fn meter_bills_work_not_sleep() {
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

        // 50 ms of sleep must not look like 50 ms of CPU. This is the whole of
        // "wall-clock, park time and socket-open time are never metered"
        // (§11.2) reduced to one assertion.
        assert!(slept < 5_000_000, "sleeping recorded {slept} ns of CPU");
        assert!(burned > 40_000_000, "50 ms of spinning recorded only {burned} ns");
    }
}
