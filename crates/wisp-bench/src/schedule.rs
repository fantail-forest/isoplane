//! Open-loop arrival schedules and pacing helpers.

use serde::{Deserialize, Serialize};

/// SplitMix64 produces repeatable sequences for an explicit seed.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    /// Uniform in (0, 1). Open at both ends: a 0 would make `ln` diverge.
    #[inline]
    pub fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        // Multiply-shift mapping avoids integer division in the sampling path.
        ((self.next_u64() as u128 * n as u128) >> 64) as u64
    }
}

/// How arrivals are spaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Arrival {
    /// Exponential inter-arrival times.
    Poisson,
    /// Exactly `1/rate` apart.
    Uniform,
}

/// One generator thread's arrival stream.
///
/// Yields nanosecond offsets from the run's `t0`, strictly increasing. There
/// is no `Instant` in here and no clock read: the whole schedule is arithmetic,
/// which is what lets it be generated ahead of the run, checkpointed, and
/// replayed.
#[derive(Debug, Clone)]
pub struct ArrivalStream {
    kind: Arrival,
    /// Mean inter-arrival, in nanoseconds.
    mean_ns: f64,
    next_ns: f64,
    rng: Rng,
}

impl ArrivalStream {
    /// `rate_hz` is this stream's own rate, already divided by the number of
    /// generator threads.
    pub fn new(kind: Arrival, rate_hz: f64, seed: u64) -> Self {
        assert!(rate_hz > 0.0, "arrival rate must be positive");
        Self { kind, mean_ns: 1e9 / rate_hz, next_ns: 0.0, rng: Rng::new(seed) }
    }

    /// The offset of the next arrival, in nanoseconds from `t0`.
    #[inline]
    pub fn next_offset_ns(&mut self) -> u64 {
        let gap = match self.kind {
            Arrival::Uniform => self.mean_ns,
            // Inverse-transform: -mean * ln(U) is Exp(1/mean).
            Arrival::Poisson => -self.mean_ns * self.rng.unit().ln(),
        };
        self.next_ns += gap;
        self.next_ns as u64
    }

    /// A uniform draw over `n`, for choosing which instance an arrival is
    /// addressed to. Shares the stream's RNG so one seed reproduces both the
    /// timing and the targeting.
    #[inline]
    pub fn pick(&mut self, n: u64) -> u64 {
        self.rng.below(n)
    }
}

/// A monotonic clock reading in nanoseconds for absolute pacer deadlines.
#[inline]
pub fn mono_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Waits until an absolute monotonic deadline and records lateness.
///
/// Absolute sleeps keep delays from accumulating across arrivals. The final
/// [`SPIN_MARGIN_NS`] interval uses busy waiting.
pub struct Pacer {
    t0_ns: u64,
    late_ns: crate::hist::Histogram,
    /// Arrivals whose deadline had already passed when we got to them: the
    /// generator is behind, and every subsequent latency includes that.
    backlogged: u64,
    fired: u64,
}

/// How long before the deadline to stop sleeping and start spinning.
pub const SPIN_MARGIN_NS: u64 = 250_000;

impl Pacer {
    pub fn new(t0_ns: u64) -> Self {
        Self { t0_ns, late_ns: crate::hist::Histogram::new(), backlogged: 0, fired: 0 }
    }

    pub fn t0(&self) -> u64 {
        self.t0_ns
    }

    /// Longest single sleep between checks of the caller's abort predicate.
    const MAX_SLEEP_NS: u64 = 200_000_000;

    /// Block until `t0 + offset_ns`, then return the actual monotonic time.
    ///
    /// Never sleeps past the deadline on purpose and never *shifts* it: if we
    /// arrive late the arrival still counts as having been due at
    /// `t0 + offset_ns`, which is what keeps the latency figure free of
    /// coordinated omission.
    #[inline]
    pub fn wait_until(&mut self, offset_ns: u64) -> u64 {
        self.wait_until_or(offset_ns, &|| false).0
    }

    /// As [`Pacer::wait_until`], but returns early when `abort` becomes true.
    ///
    /// The second element of the return is whether it aborted; an aborted wait
    /// records nothing, because no arrival was released.
    pub fn wait_until_or(&mut self, offset_ns: u64, abort: &dyn Fn() -> bool) -> (u64, bool) {
        let deadline = self.t0_ns + offset_ns;
        let mut now = mono_ns();
        if now >= deadline {
            self.backlogged += 1;
            self.fired += 1;
            self.late_ns.record(now - deadline);
            return (now, false);
        }
        while deadline - now > SPIN_MARGIN_NS {
            if abort() {
                return (now, true);
            }
            let target = (deadline - SPIN_MARGIN_NS).min(now + Self::MAX_SLEEP_NS);
            let ts = libc::timespec {
                tv_sec: (target / 1_000_000_000) as libc::time_t,
                tv_nsec: (target % 1_000_000_000) as i64,
            };
            unsafe {
                libc::clock_nanosleep(
                    libc::CLOCK_MONOTONIC,
                    libc::TIMER_ABSTIME,
                    &ts,
                    std::ptr::null_mut(),
                );
            }
            now = mono_ns();
        }
        while now < deadline {
            std::hint::spin_loop();
            now = mono_ns();
        }
        self.fired += 1;
        self.late_ns.record(now - deadline);
        (now, false)
    }

    /// The distribution of `(actual release - scheduled arrival)`.
    pub fn lateness(&self) -> crate::hist::HistSnapshot {
        self.late_ns.snapshot()
    }

    pub fn backlogged(&self) -> u64 {
        self.backlogged
    }
    pub fn fired(&self) -> u64 {
        self.fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_reproduces_a_schedule_exactly() {
        let mut a = ArrivalStream::new(Arrival::Poisson, 1000.0, 42);
        let mut b = ArrivalStream::new(Arrival::Poisson, 1000.0, 42);
        for _ in 0..1000 {
            assert_eq!(a.next_offset_ns(), b.next_offset_ns());
            assert_eq!(a.pick(97), b.pick(97));
        }
        let mut c = ArrivalStream::new(Arrival::Poisson, 1000.0, 43);
        assert_ne!(a.next_offset_ns(), c.next_offset_ns());
    }

    #[test]
    fn arrivals_are_monotonic_and_hit_the_requested_rate() {
        for kind in [Arrival::Poisson, Arrival::Uniform] {
            let mut s = ArrivalStream::new(kind, 10_000.0, 7);
            let n = 200_000u64;
            let mut last = 0;
            for _ in 0..n {
                let t = s.next_offset_ns();
                assert!(t >= last, "{kind:?} arrivals must not go backwards");
                last = t;
            }
            let achieved = n as f64 / (last as f64 / 1e9);
            assert!(
                (achieved - 10_000.0).abs() / 10_000.0 < 0.02,
                "{kind:?} produced {achieved:.0}/s, wanted 10000/s"
            );
        }
    }

    /// Exponential gaps vary around their mean; uniform gaps remain fixed.
    #[test]
    fn poisson_gaps_are_actually_dispersed() {
        let mut s = ArrivalStream::new(Arrival::Poisson, 1000.0, 11);
        let mut gaps = Vec::new();
        let mut last = 0u64;
        for _ in 0..100_000 {
            let t = s.next_offset_ns();
            gaps.push((t - last) as f64);
            last = t;
        }
        let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
        let var = gaps.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / gaps.len() as f64;
        let cv = var.sqrt() / mean;
        assert!((cv - 1.0).abs() < 0.05, "exponential gaps should have CV ~= 1, got {cv:.3}");

        let mut u = ArrivalStream::new(Arrival::Uniform, 1000.0, 11);
        let mut last = 0u64;
        let mut ugaps = Vec::new();
        for _ in 0..1000 {
            let t = u.next_offset_ns();
            ugaps.push((t - last) as f64);
            last = t;
        }
        let umean = ugaps.iter().sum::<f64>() / ugaps.len() as f64;
        let uvar = ugaps.iter().map(|g| (g - umean).powi(2)).sum::<f64>() / ugaps.len() as f64;
        assert!(uvar.sqrt() / umean < 0.01, "uniform gaps should be fixed");
    }

    /// A wait for a distant arrival observes cancellation between sleeps.
    #[test]
    fn a_wait_for_a_distant_arrival_still_notices_a_stop() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let t0 = mono_ns();
        let h = std::thread::spawn(move || {
            let mut p = Pacer::new(t0);
            // A year away.
            let (_, aborted) = p.wait_until_or(31_536_000_000_000_000, &|| s2.load(Ordering::Relaxed));
            (aborted, p.fired())
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        stop.store(true, Ordering::Relaxed);
        let (aborted, fired) = h.join().expect("the pacer must not hang");
        assert!(aborted, "the wait must report that it aborted");
        assert_eq!(fired, 0, "an aborted wait released no arrival and must record none");
    }

    #[test]
    fn the_pacer_does_not_let_lateness_accumulate() {
        let t0 = mono_ns();
        let mut p = Pacer::new(t0);
        // 200 arrivals 200 µs apart = 40 ms of wall clock.
        for i in 1..=200u64 {
            p.wait_until(i * 200_000);
        }
        let total = mono_ns() - t0;
        assert!(
            total < 200 * 200_000 + 20_000_000,
            "200 paced arrivals took {}ms, schedule was 40ms",
            total / 1_000_000
        );
        let late = p.lateness();
        assert_eq!(late.count, 200);
        // The interesting assertion is not "fast" but "not drifting": the last
        // arrival must not be late by the sum of the earlier slips.
        assert!(
            total as i64 - (200 * 200_000) as i64 > -1_000_000,
            "finished before the schedule, which means the pacer is not pacing"
        );
    }
}
