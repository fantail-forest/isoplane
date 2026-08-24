//! Quantile thresholds for evaluating a latency distribution.

use serde::{Deserialize, Serialize};

use crate::hist::Percentiles;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Slo {
    pub t75_ns: u64,
    pub t95_ns: u64,
    pub t99_ns: u64,
}

impl Slo {
    pub fn us(t75: u64, t95: u64, t99: u64) -> Self {
        Self { t75_ns: t75 * 1_000, t95_ns: t95 * 1_000, t99_ns: t99 * 1_000 }
    }
    pub fn ms(t75: u64, t95: u64, t99: u64) -> Self {
        Self { t75_ns: t75 * 1_000_000, t95_ns: t95 * 1_000_000, t99_ns: t99 * 1_000_000 }
    }

    pub fn label(&self) -> String {
        format!(
            "({:.3}, {:.3}, {:.3}) ms",
            self.t75_ns as f64 / 1e6,
            self.t95_ns as f64 / 1e6,
            self.t99_ns as f64 / 1e6
        )
    }

    /// Judge one distribution. Every quantile's headroom is reported whether
    /// it passed or not, because *how close* a passing point was is what makes
    /// a knee locatable without running every intermediate point.
    pub fn judge(&self, p: &Percentiles) -> SloVerdict {
        let h = |measured: f64, target: u64| measured / target as f64;
        SloVerdict {
            pass: p.p75_ns <= self.t75_ns as f64
                && p.p95_ns <= self.t95_ns as f64
                && p.p99_ns <= self.t99_ns as f64,
            ratio_p75: h(p.p75_ns, self.t75_ns),
            ratio_p95: h(p.p95_ns, self.t95_ns),
            ratio_p99: h(p.p99_ns, self.t99_ns),
        }
    }
}

/// The outcome of judging one distribution against one vector.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SloVerdict {
    pub pass: bool,
    /// measured / target. 1.0 is exactly at the limit; > 1.0 is a breach.
    pub ratio_p75: f64,
    pub ratio_p95: f64,
    pub ratio_p99: f64,
}

impl SloVerdict {
    /// The quantile that is closest to (or furthest past) its target. This is
    /// the answer to "which end of the distribution bent", and it is a
    /// different diagnosis each way: a p75 breach with a healthy p99 is the
    /// whole body shifting — service time went up — while a p99 breach with a
    /// healthy p75 is a queue forming behind an occasional stall.
    pub fn binding(&self) -> &'static str {
        let m = self.ratio_p75.max(self.ratio_p95).max(self.ratio_p99);
        if m == self.ratio_p99 {
            "p99"
        } else if m == self.ratio_p95 {
            "p95"
        } else {
            "p75"
        }
    }

    pub fn worst_ratio(&self) -> f64 {
        self.ratio_p75.max(self.ratio_p95).max(self.ratio_p99)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(p75: f64, p95: f64, p99: f64) -> Percentiles {
        Percentiles {
            p50_ns: p75 / 2.0,
            p75_ns: p75,
            p95_ns: p95,
            p99_ns: p99,
            p999_ns: p99 * 2.0,
            max_ns: p99 * 4.0,
            mean_ns: p75,
            count: 1_000_000,
        }
    }

    /// The whole point of a vector rather than a scalar: an excellent body
    /// does not buy a failing tail.
    #[test]
    fn all_three_must_clear() {
        let t = Slo::ms(1, 5, 10);
        assert!(t.judge(&p(500_000.0, 2_000_000.0, 9_000_000.0)).pass);
        let v = t.judge(&p(100_000.0, 200_000.0, 40_000_000.0));
        assert!(!v.pass, "a 4x p99 breach must fail however good the body is");
        assert_eq!(v.binding(), "p99");
        let v = t.judge(&p(4_000_000.0, 4_100_000.0, 4_200_000.0));
        assert!(!v.pass, "a p75 breach must fail however good the tail is");
        assert_eq!(v.binding(), "p75");
    }

    #[test]
    fn exactly_at_the_limit_passes() {
        let t = Slo::ms(1, 5, 10);
        let v = t.judge(&p(1e6, 5e6, 1e7));
        assert!(v.pass);
        assert!((v.worst_ratio() - 1.0).abs() < 1e-9);
    }
}
