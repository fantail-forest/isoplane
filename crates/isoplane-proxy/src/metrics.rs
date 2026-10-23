//! Proxy-side meters: the outermost layer of a request's latency
//! decomposition, since only the proxy knows how long a request waited behind
//! the admission queue, and which code tier served it.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use isoplane_abi::host::CodeTier;
use parking_lot::Mutex;

/// A bounded-memory lifetime distribution over microseconds, with four buckets
/// per power of two and inclusive bucket-upper quantiles. Recording stops if
/// the lifetime count reaches `u64::MAX`.
#[derive(Debug)]
pub struct Histogram {
    state: Mutex<HistogramState>,
}

#[derive(Debug)]
struct HistogramState {
    distribution: histogram::Histogram,
    count: u64,
    sum_us: u128,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    /// Creates an empty histogram covering every `u64` microsecond value.
    pub fn new() -> Self {
        let distribution = histogram::Histogram::new(2, 64)
            .expect("grouping power 2 is below maximum power 64, within the supported range");
        Self {
            state: Mutex::new(HistogramState {
                distribution,
                count: 0,
                sum_us: 0,
            }),
        }
    }

    /// Records one microsecond observation without narrowing its range.
    pub fn record_us(&self, us: u64) {
        let mut state = self.state.lock();
        let Some(count) = state.count.checked_add(1) else {
            tracing::warn!("proxy histogram lifetime count exhausted; observation dropped");
            return;
        };
        if let Err(error) = state.distribution.increment(us) {
            tracing::error!(%error, us, "proxy histogram rejected an observation");
            return;
        }
        state.count = count;
        state.sum_us += u128::from(us);
    }

    /// Records a duration, saturating values beyond `u64::MAX` microseconds.
    pub fn record(&self, duration: std::time::Duration) {
        self.record_us(duration.as_micros().min(u128::from(u64::MAX)) as u64);
    }

    /// Returns the number of accepted lifetime observations.
    pub fn count(&self) -> u64 {
        self.state.lock().count
    }

    /// Returns the sum of accepted lifetime observations in microseconds.
    pub fn sum_us(&self) -> u128 {
        self.state.lock().sum_us
    }

    /// Returns the lifetime arithmetic mean in microseconds, or zero if empty.
    pub fn mean_us(&self) -> f64 {
        let state = self.state.lock();
        if state.count == 0 {
            0.0
        } else {
            state.sum_us as f64 / state.count as f64
        }
    }

    fn quantile_us(&self, quantile: f64) -> f64 {
        match self.state.lock().distribution.quantile_bucket(quantile) {
            Ok(Some(bucket)) => bucket.end() as f64,
            Ok(None) => 0.0,
            Err(error) => {
                tracing::error!(%error, quantile, "proxy histogram rejected a quantile");
                0.0
            }
        }
    }

    /// Returns the median bucket's inclusive upper bound, or zero if empty.
    pub fn p50_us(&self) -> f64 {
        self.quantile_us(0.50)
    }

    /// Returns the 99th-percentile bucket's inclusive upper bound, or zero if empty.
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
    fn a_single_value_is_never_above_its_quantile_bucket() {
        for value_us in [0, 1, 2, 3, 95, u64::MAX] {
            let metrics = ProxyMetrics::new();
            metrics.request_latency.record_us(value_us);
            assert!(
                metrics.request_latency.p99_us() >= value_us as f64,
                "p99 {} is below its only observation {value_us}",
                metrics.request_latency.p99_us()
            );
            assert_eq!(metrics.request_latency.count(), 1);
        }
    }

    #[test]
    fn large_observations_keep_their_totals_and_storage_is_bounded() {
        let metrics = ProxyMetrics::new();
        let histogram = &metrics.request_latency;
        let buckets = histogram.state.lock().distribution.config().total_buckets();
        for _ in 0..1000 {
            histogram.record_us(u64::MAX);
        }
        assert_eq!(histogram.count(), 1000);
        assert_eq!(histogram.mean_us(), u64::MAX as f64);
        assert_eq!(histogram.p99_us(), u64::MAX as f64);
        assert_eq!(
            histogram.state.lock().distribution.config().total_buckets(),
            buckets
        );
        assert!(buckets <= 256, "four buckets per power of two");
    }

    #[test]
    fn durations_beyond_the_microsecond_range_saturate() {
        let histogram = Histogram::new();
        histogram.record(std::time::Duration::MAX);
        assert_eq!(histogram.count(), 1);
        assert_eq!(histogram.mean_us(), u64::MAX as f64);
        assert_eq!(histogram.p99_us(), u64::MAX as f64);
    }

    #[test]
    fn concurrent_observations_preserve_their_counts_and_mean() {
        let metrics = ProxyMetrics::new();
        let ready = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            for value_us in 100..104 {
                let ready = &ready;
                let histogram = &metrics.request_latency;
                scope.spawn(move || {
                    ready.wait();
                    for _ in 0..1000 {
                        histogram.record_us(value_us);
                    }
                });
            }
            ready.wait();
        });
        assert_eq!(metrics.request_latency.count(), 4000);
        assert_eq!(metrics.request_latency.mean_us(), 101.5);
        assert!(metrics.request_latency.p99_us() >= 103.0);
    }

    #[test]
    fn an_exhausted_count_keeps_the_last_valid_distribution() {
        let histogram = Histogram::new();
        {
            let mut state = histogram.state.lock();
            state.distribution.add(u64::MAX, u64::MAX - 1).unwrap();
            state.count = u64::MAX - 1;
            state.sum_us = u128::from(u64::MAX - 1) * u128::from(u64::MAX);
        }
        histogram.record_us(u64::MAX);
        histogram.record_us(0);
        assert_eq!(histogram.count(), u64::MAX);
        assert_eq!(histogram.mean_us(), u64::MAX as f64);
        assert_eq!(histogram.p99_us(), u64::MAX as f64);
    }

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
