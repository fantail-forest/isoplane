//! Proxy-side meters: the outermost layer of a request's latency
//! decomposition, since only the proxy knows how long a request waited behind
//! the admission queue, and which code tier served it.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use isoplane_abi::host::CodeTier;

/// A fixed-bucket log histogram over microseconds: four buckets per octave,
/// updated lock-free from every request thread.
#[derive(Debug)]
pub struct Histogram {
    buckets: [AtomicU64; Self::N],
    count: AtomicU64,
    sum_us: AtomicU64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    const N: usize = 128;

    pub fn new() -> Self {
        Histogram {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum_us: AtomicU64::new(0),
        }
    }

    fn bucket_of(us: u64) -> usize {
        if us == 0 {
            return 0;
        }
        let hi = 63 - us.leading_zeros() as usize;
        let frac = ((us >> hi.saturating_sub(2)) & 0b11) as usize;
        let idx = if hi >= 2 { hi * 4 + frac } else { hi * 4 };
        idx.min(Self::N - 1)
    }

    fn bucket_upper_us(i: usize) -> f64 {
        let octave = (i / 4) as f64;
        let frac = (i % 4) as f64;
        2f64.powf(octave + (frac + 1.0) / 4.0)
    }

    pub fn record_us(&self, us: u64) {
        self.buckets[Self::bucket_of(us)].fetch_add(1, Relaxed);
        self.count.fetch_add(1, Relaxed);
        self.sum_us.fetch_add(us, Relaxed);
    }

    pub fn record(&self, d: std::time::Duration) {
        self.record_us(d.as_micros() as u64);
    }

    pub fn count(&self) -> u64 {
        self.count.load(Relaxed)
    }

    pub fn mean_us(&self) -> f64 {
        let c = self.count();
        if c == 0 {
            0.0
        } else {
            self.sum_us.load(Relaxed) as f64 / c as f64
        }
    }

    /// Upper edge of the bucket containing the `q` quantile, in microseconds.
    pub fn quantile_us(&self, q: f64) -> f64 {
        let total = self.count();
        if total == 0 {
            return 0.0;
        }
        let want = (total as f64 * q).ceil() as u64;
        let mut seen = 0u64;
        for (i, bucket) in self.buckets.iter().enumerate() {
            seen += bucket.load(Relaxed);
            if seen >= want {
                return Self::bucket_upper_us(i);
            }
        }
        Self::bucket_upper_us(Self::N - 1)
    }

    pub fn p50_us(&self) -> f64 {
        self.quantile_us(0.50)
    }
    pub fn p99_us(&self) -> f64 {
        self.quantile_us(0.99)
    }
}

/// What the proxy layer knows. Cell-side meters live in the cell and are
/// joined by `isoplaned`.
#[derive(Debug, Default)]
pub struct ProxyMetrics {
    pub requests_total: AtomicU64,
    pub requests_404: AtomicU64,
    pub requests_shed: AtomicU64,
    pub requests_failed: AtomicU64,
    pub dispatches_tier_fast: AtomicU64,
    pub dispatches_tier_optimized: AtomicU64,

    pub route_resolutions_total: AtomicU64,
    pub route_generation: AtomicU64,

    /// End to end, as the proxy sees it.
    pub request_latency: Histogram,
    /// Time in the admission queue before a permit was granted.
    pub admission_wait: Histogram,
}

impl ProxyMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn note_tier(&self, tier: CodeTier) {
        match tier {
            CodeTier::Fast => &self.dispatches_tier_fast,
            CodeTier::Optimized => &self.dispatches_tier_optimized,
        }
        .fetch_add(1, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_quantiles_are_within_one_bucket() {
        let h = Histogram::new();
        for i in 1..=1000u64 {
            h.record_us(i);
        }
        let p50 = h.p50_us();
        assert!(
            (400.0..=640.0).contains(&p50),
            "p50 of 1..=1000 must land in the bucket around 500, was {p50}"
        );
        let p99 = h.p99_us();
        assert!(
            (960.0..=1300.0).contains(&p99),
            "p99 of 1..=1000 must land in the bucket around 990, was {p99}"
        );
    }

    #[test]
    fn histogram_is_monotone_in_the_quantile() {
        let h = Histogram::new();
        for i in [1u64, 5, 50, 500, 5_000, 50_000] {
            h.record_us(i);
        }
        assert!(h.p50_us() <= h.p99_us(), "p50 cannot exceed p99");
        assert_eq!(h.count(), 6, "every sample is counted");
    }

    #[test]
    fn an_empty_histogram_reports_zero_not_nan() {
        let h = Histogram::new();
        assert_eq!(h.p50_us(), 0.0, "no samples is a zero quantile");
        assert_eq!(h.mean_us(), 0.0, "no samples is a zero mean");
    }
}
