//! Latency histograms and a bounded reservoir for examining distribution shape.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Sub-buckets per power-of-two magnitude. Within the represented range,
/// midpoint estimates have about 0.2% worst-case relative quantization error.
const SUB_BITS: u32 = 8;
const SUB: usize = 1 << SUB_BITS;
/// Magnitudes covered: 2^0 ns through 2^47 ns (~39 hours).
const MAGNITUDES: usize = 48;
const BUCKETS: usize = MAGNITUDES * SUB;

/// Worst-case relative error of a value read back out of a bucket.
pub const PRECISION_PCT: f64 = 100.0 / (2.0 * SUB as f64);

/// A concurrent log-linear histogram over nanosecond values.
pub struct Histogram {
    buckets: Vec<AtomicU64>,
    count: AtomicU64,
    sum: AtomicU64,
    min: AtomicU64,
    max: AtomicU64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

/// Which bucket a value falls in. Values below `SUB` have exact 1 ns buckets.
#[inline]
fn bucket_of(v: u64) -> usize {
    if v < SUB as u64 {
        return v as usize;
    }
    let msb = 63 - v.leading_zeros() as usize; // >= SUB_BITS
    let shift = msb - SUB_BITS as usize;
    let sub = ((v >> shift) & (SUB as u64 - 1)) as usize;
    let idx = (shift + 1) * SUB + sub;
    idx.min(BUCKETS - 1)
}

/// The value at the *midpoint* of a bucket, which is the least-biased estimate
/// available once the exact value is gone.
#[inline]
fn value_of(idx: usize) -> f64 {
    if idx < SUB {
        return idx as f64;
    }
    let shift = idx / SUB - 1;
    let sub = idx % SUB;
    let base = ((SUB + sub) as u64) << shift;
    let width = 1u64 << shift;
    base as f64 + width as f64 / 2.0
}

impl Histogram {
    pub fn new() -> Self {
        Self {
            buckets: (0..BUCKETS).map(|_| AtomicU64::new(0)).collect(),
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            min: AtomicU64::new(u64::MAX),
            max: AtomicU64::new(0),
        }
    }

    /// Record one observation in nanoseconds.
    ///
    /// Counters use relaxed ordering; a concurrent snapshot may observe their
    /// updates at different instants.
    #[inline]
    pub fn record(&self, ns: u64) {
        self.buckets[bucket_of(ns)].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(ns, Ordering::Relaxed);
        self.min.fetch_min(ns, Ordering::Relaxed);
        self.max.fetch_max(ns, Ordering::Relaxed);
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> HistSnapshot {
        HistSnapshot {
            buckets: self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).collect(),
            count: self.count.load(Ordering::Relaxed),
            sum: self.sum.load(Ordering::Relaxed),
            min: self.min.load(Ordering::Relaxed),
            max: self.max.load(Ordering::Relaxed),
        }
    }
}

/// A histogram snapshot. Subtract two snapshots to describe an observation window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistSnapshot {
    buckets: Vec<u64>,
    pub count: u64,
    pub sum: u64,
    min: u64,
    max: u64,
}

impl HistSnapshot {
    pub fn empty() -> Self {
        Self { buckets: vec![0; BUCKETS], count: 0, sum: 0, min: u64::MAX, max: 0 }
    }

    /// `self - base`, i.e. the observations recorded between the two
    /// snapshots.
    ///
    /// `min`/`max` cannot be differenced — an extremum recorded before `base`
    /// is not removable — so the window reports them as the window's own
    /// bucket extremes, which is the honest available answer and is why they
    /// are not public fields.
    pub fn since(&self, base: &HistSnapshot) -> HistSnapshot {
        let buckets: Vec<u64> = self
            .buckets
            .iter()
            .zip(&base.buckets)
            .map(|(a, b)| a.saturating_sub(*b))
            .collect();
        let first = buckets.iter().position(|c| *c > 0);
        let last = buckets.iter().rposition(|c| *c > 0);
        HistSnapshot {
            count: self.count.saturating_sub(base.count),
            sum: self.sum.saturating_sub(base.sum),
            min: first.map(|i| value_of(i) as u64).unwrap_or(u64::MAX),
            max: last.map(|i| value_of(i) as u64).unwrap_or(0),
            buckets,
        }
    }

    /// The value at quantile `q` (0.0..=1.0), in nanoseconds, or `None` if
    /// nothing was recorded.
    pub fn quantile(&self, q: f64) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        // Nearest-rank on the cumulative count. `ceil` so that q=1.0 lands on
        // the last observation rather than one past it.
        let rank = ((q.clamp(0.0, 1.0) * self.count as f64).ceil() as u64).max(1);
        let mut seen = 0u64;
        for (i, c) in self.buckets.iter().enumerate() {
            seen += *c;
            if seen >= rank {
                return Some(value_of(i));
            }
        }
        Some(value_of(self.buckets.len() - 1))
    }

    pub fn p50(&self) -> Option<f64> {
        self.quantile(0.50)
    }
    pub fn p75(&self) -> Option<f64> {
        self.quantile(0.75)
    }
    pub fn p95(&self) -> Option<f64> {
        self.quantile(0.95)
    }
    pub fn p99(&self) -> Option<f64> {
        self.quantile(0.99)
    }
    pub fn p999(&self) -> Option<f64> {
        self.quantile(0.999)
    }
    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum as f64 / self.count as f64)
    }
    pub fn min(&self) -> Option<f64> {
        (self.count > 0 && self.min != u64::MAX).then_some(self.min as f64)
    }
    pub fn max(&self) -> Option<f64> {
        (self.count > 0).then_some(self.max as f64)
    }

    /// Minimum sample count used by the quantile confidence check.
    pub fn confident_at(&self, q: f64) -> bool {
        let needed = (10.0 / (1.0 - q).max(1e-9)).ceil() as u64;
        self.count >= needed
    }

    /// A compact triple for the report: the SLO vector's three quantiles.
    pub fn triple(&self) -> Option<Percentiles> {
        Some(Percentiles {
            p75_ns: self.p75()?,
            p95_ns: self.p95()?,
            p99_ns: self.p99()?,
            p50_ns: self.p50()?,
            p999_ns: self.p999()?,
            max_ns: self.max()?,
            mean_ns: self.mean()?,
            count: self.count,
        })
    }
}

/// The `(p75, p95, p99)` quantiles and distribution summary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Percentiles {
    pub p50_ns: f64,
    pub p75_ns: f64,
    pub p95_ns: f64,
    pub p99_ns: f64,
    pub p999_ns: f64,
    pub max_ns: f64,
    pub mean_ns: f64,
    pub count: u64,
}

impl Percentiles {
    pub fn fmt_us(&self) -> String {
        format!(
            "p50 {:.0}µs p75 {:.0}µs p95 {:.0}µs p99 {:.0}µs p99.9 {:.0}µs max {:.0}µs (n={})",
            self.p50_ns / 1e3,
            self.p75_ns / 1e3,
            self.p95_ns / 1e3,
            self.p99_ns / 1e3,
            self.p999_ns / 1e3,
            self.max_ns / 1e3,
            self.count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_values_are_exact() {
        for v in 0..SUB as u64 {
            assert_eq!(value_of(bucket_of(v)), v as f64, "value {v} should be exact");
        }
    }

    #[test]
    fn large_values_are_within_the_advertised_precision() {
        for v in [1_000u64, 12_345, 1_000_000, 50_000_000, 3_000_000_000] {
            let got = value_of(bucket_of(v));
            let err = (got - v as f64).abs() / v as f64 * 100.0;
            assert!(err <= PRECISION_PCT, "value {v} -> {got} is {err:.4}% off");
        }
    }

    #[test]
    fn quantiles_come_off_one_distribution() {
        let h = Histogram::new();
        // 1..=10_000 microseconds, one sample each.
        for i in 1..=10_000u64 {
            h.record(i * 1_000);
        }
        let s = h.snapshot();
        let t = s.triple().unwrap();
        for (q, got) in [(0.75, t.p75_ns), (0.95, t.p95_ns), (0.99, t.p99_ns)] {
            let want = q * 10_000.0 * 1_000.0;
            let err = (got - want).abs() / want * 100.0;
            assert!(err < 1.0, "q{q} = {got} ns, expected ~{want} ns ({err:.3}% off)");
        }
        assert_eq!(t.count, 10_000);
    }

    /// A window contains only observations recorded since its base snapshot.
    #[test]
    fn a_window_sees_only_its_own_observations() {
        let h = Histogram::new();
        for _ in 0..1_000 {
            h.record(1_000);
        }
        let base = h.snapshot();
        for _ in 0..1_000 {
            h.record(100_000);
        }
        let w = h.snapshot().since(&base);
        assert_eq!(w.count, 1_000);
        let p50 = w.p50().unwrap();
        assert!((p50 - 100_000.0).abs() / 100_000.0 < 0.01, "window p50 was {p50}");
        // And the cumulative view still sees both halves.
        assert_eq!(h.snapshot().count, 2_000);
    }

    #[test]
    fn confidence_refuses_a_p99_from_fifty_samples() {
        let h = Histogram::new();
        for _ in 0..50 {
            h.record(1_000);
        }
        let s = h.snapshot();
        assert!(!s.confident_at(0.99));
        assert!(s.confident_at(0.75));
    }
}
