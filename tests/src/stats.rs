//! Empirical quantiles, sample summaries and sample-size heuristics.

use std::fmt;

/// A finished set of observations, sorted, with its sample count attached for
/// life.
#[derive(Debug, Clone)]
pub struct Summary {
    /// Sorted ascending.
    sorted: Vec<f64>,
    unit: &'static str,
}

/// How much weight a quantile from this sample can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// `n` is comfortably above `10 / (1 - q)`.
    Ok,
    /// The quantile is computed from too few samples to be stable. Still
    /// reported — with a marker — because a weak number beats no number, but
    /// it must never be laundered into a clean one.
    UnderPowered { need: usize, have: usize },
}

impl Confidence {
    pub fn marker(self) -> &'static str {
        match self {
            Confidence::Ok => "",
            Confidence::UnderPowered { .. } => "~",
        }
    }
    pub fn is_ok(self) -> bool {
        matches!(self, Confidence::Ok)
    }
}

impl Summary {
    /// Consumes raw observations. NaNs are dropped and counted out of `n`
    /// rather than poisoning the sort.
    pub fn new(mut samples: Vec<f64>, unit: &'static str) -> Self {
        samples.retain(|v| v.is_finite());
        samples.sort_by(|a, b| a.partial_cmp(b).expect("NaNs were filtered"));
        Self { sorted: samples, unit }
    }

    pub fn from_nanos(samples: impl IntoIterator<Item = u64>) -> Self {
        Self::new(samples.into_iter().map(|n| n as f64 / 1e6).collect(), "ms")
    }

    pub fn from_micros(samples: impl IntoIterator<Item = u64>) -> Self {
        Self::new(samples.into_iter().map(|n| n as f64 / 1e3).collect(), "ms")
    }

    pub fn from_bytes(samples: impl IntoIterator<Item = u64>) -> Self {
        Self::new(samples.into_iter().map(|n| n as f64).collect(), "B")
    }

    pub fn n(&self) -> usize {
        self.sorted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    pub fn unit(&self) -> &'static str {
        self.unit
    }

    /// Hyndman-Fan type 7: `h = (n - 1) * q`, linear between `floor(h)` and
    /// `ceil(h)`. Returns `None` for an empty sample rather than 0.0, because
    /// "no data" and "zero" are different claims and this harness must not
    /// confuse them.
    pub fn quantile(&self, q: f64) -> Option<f64> {
        if self.sorted.is_empty() {
            return None;
        }
        let q = q.clamp(0.0, 1.0);
        let h = (self.sorted.len() - 1) as f64 * q;
        let lo = h.floor() as usize;
        let hi = h.ceil() as usize;
        let frac = h - lo as f64;
        Some(self.sorted[lo] + (self.sorted[hi] - self.sorted[lo]) * frac)
    }

    /// Samples needed before the empirical `q`-quantile is worth quoting.
    pub fn samples_needed(q: f64) -> usize {
        if q <= 0.5 {
            20
        } else {
            (10.0 / (1.0 - q)).ceil() as usize
        }
    }

    pub fn confidence(&self, q: f64) -> Confidence {
        let need = Self::samples_needed(q);
        if self.sorted.len() >= need {
            Confidence::Ok
        } else {
            Confidence::UnderPowered { need, have: self.sorted.len() }
        }
    }

    pub fn p50(&self) -> Option<f64> {
        self.quantile(0.50)
    }
    pub fn p99(&self) -> Option<f64> {
        self.quantile(0.99)
    }
    pub fn min(&self) -> Option<f64> {
        self.sorted.first().copied()
    }
    pub fn max(&self) -> Option<f64> {
        self.sorted.last().copied()
    }

    pub fn mean(&self) -> Option<f64> {
        if self.sorted.is_empty() {
            return None;
        }
        Some(self.sorted.iter().sum::<f64>() / self.sorted.len() as f64)
    }

    /// Sample standard deviation (Bessel-corrected).
    pub fn stddev(&self) -> Option<f64> {
        if self.sorted.len() < 2 {
            return None;
        }
        let m = self.mean()?;
        let ss: f64 = self.sorted.iter().map(|v| (v - m) * (v - m)).sum();
        Some((ss / (self.sorted.len() - 1) as f64).sqrt())
    }

    /// The one-line form used everywhere in the report. `n` is not optional.
    pub fn fmt_line(&self) -> String {
        if self.sorted.is_empty() {
            return "no samples".into();
        }
        let c50 = self.confidence(0.50).marker();
        let c99 = self.confidence(0.99).marker();
        format!(
            "n={} p50={}{:.3}{u} p99={}{:.3}{u} min={:.3}{u} max={:.3}{u}",
            self.n(),
            c50,
            self.p50().unwrap_or_default(),
            c99,
            self.p99().unwrap_or_default(),
            self.min().unwrap_or_default(),
            self.max().unwrap_or_default(),
            u = self.unit,
        )
    }

    /// A coarse ASCII histogram. Useful in the demo output because S2 asks for
    /// "the histogram", and a shape is worth more than three quantiles when
    /// the question is whether a distribution is bimodal (warm reuse vs cold
    /// activation would be).
    pub fn histogram(&self, buckets: usize, width: usize) -> String {
        if self.sorted.is_empty() {
            return "no samples".into();
        }
        let (lo, hi) = (self.min().unwrap(), self.max().unwrap());
        if !(hi > lo) {
            return format!("all {} samples at {:.3}{}", self.n(), lo, self.unit);
        }
        let mut counts = vec![0usize; buckets];
        let span = hi - lo;
        for v in &self.sorted {
            let mut i = (((v - lo) / span) * buckets as f64) as usize;
            if i >= buckets {
                i = buckets - 1;
            }
            counts[i] += 1;
        }
        let peak = counts.iter().copied().max().unwrap_or(1).max(1);
        let mut out = String::new();
        for (i, c) in counts.iter().enumerate() {
            let edge = lo + span * (i as f64) / buckets as f64;
            let bar = (c * width).div_ceil(peak);
            out.push_str(&format!(
                "      {:>9.3}{:<2} | {:<width$} {}\n",
                edge,
                self.unit,
                "#".repeat(bar),
                c,
                width = width
            ));
        }
        out
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.fmt_line())
    }
}

/// Ordinary least squares through `(x, y)`, returning `(slope, intercept)`.
///
/// S1 needs this: the marginal cost of an instance is the *slope* of RSS
/// against instance count, not `RSS / n`, because the latter charges every
/// instance a share of the cell's fixed overhead and so overstates marginal
/// cost at small N — exactly the mistake that would make a density claim look
/// worse than the design is.
pub fn linear_fit(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    if points.len() < 2 {
        return None;
    }
    let n = points.len() as f64;
    let sx: f64 = points.iter().map(|p| p.0).sum();
    let sy: f64 = points.iter().map(|p| p.1).sum();
    let sxx: f64 = points.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = points.iter().map(|p| p.0 * p.1).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < f64::EPSILON {
        return None;
    }
    let slope = (n * sxy - sx * sy) / denom;
    Some((slope, (sy - slope * sx) / n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_match_the_type_7_definition() {
        // NumPy: np.percentile([1..10], 50) == 5.5, ...99) == 9.91
        let s = Summary::new((1..=10).map(|v| v as f64).collect(), "u");
        assert!((s.p50().unwrap() - 5.5).abs() < 1e-9);
        assert!((s.p99().unwrap() - 9.91).abs() < 1e-9);
        assert_eq!(s.min().unwrap(), 1.0);
        assert_eq!(s.max().unwrap(), 10.0);
    }

    #[test]
    fn an_empty_sample_yields_none_not_zero() {
        let s = Summary::new(vec![], "ms");
        assert!(s.p50().is_none(), "no data must not read as 0.0");
        assert_eq!(s.fmt_line(), "no samples");
    }

    /// The whole point of the module: ten samples cannot support a p99, and
    /// the harness must say so out loud.
    #[test]
    fn ten_samples_cannot_support_a_p99() {
        let s = Summary::new((0..10).map(|v| v as f64).collect(), "ms");
        assert_eq!(s.confidence(0.99), Confidence::UnderPowered { need: 1000, have: 10 });
        assert!(s.fmt_line().contains("p99=~"), "under-powered p99 must be marked: {}", s.fmt_line());
        // p50 at n=10 is also under-powered by the same rule.
        assert!(!s.confidence(0.50).is_ok());

        let big = Summary::new((0..1000).map(|v| v as f64).collect(), "ms");
        assert!(big.confidence(0.99).is_ok());
        assert!(!big.fmt_line().contains("p99=~"));
    }

    #[test]
    fn a_slope_is_not_an_average() {
        // 4 MB fixed overhead + 1 MB per instance. `total/n` at n=1 would say
        // 5 MB/instance; the slope says 1 MB, which is the true marginal cost.
        let pts: Vec<(f64, f64)> =
            (1..=10).map(|n| (n as f64, 4.0 + n as f64)).collect();
        let (slope, intercept) = linear_fit(&pts).unwrap();
        assert!((slope - 1.0).abs() < 1e-9);
        assert!((intercept - 4.0).abs() < 1e-9);
    }

    #[test]
    fn nans_are_dropped_not_sorted() {
        let s = Summary::new(vec![1.0, f64::NAN, 3.0], "ms");
        assert_eq!(s.n(), 2);
    }
}
