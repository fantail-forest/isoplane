//! Process memory measurements from Linux procfs.

use std::fs;
use std::io;

/// One sample of this process's memory, taken from the kernel rather than
/// from any allocator's bookkeeping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MemSample {
    /// `VmRSS` — total resident, shared pages counted once.
    pub rss_bytes: u64,
    /// `VmHWM` — peak resident. A watermark, so it never goes down; useful for
    /// catching a transient spike a periodic sampler would miss.
    pub peak_rss_bytes: u64,
    /// `Pss` from smaps_rollup — resident, with shared pages divided by the
    /// number of mappers. The machine-level cost of this process.
    pub pss_bytes: u64,
    /// `Private_Dirty` — written pages this process privately owns. **This is
    /// the C1 number**: the marginal cost of an instance under CoW.
    pub private_dirty_bytes: u64,
    pub private_clean_bytes: u64,
    /// `Shared_Clean` — the CoW template pages, mapped many times, paid for
    /// once. Growth here as N grows is the sharing working.
    pub shared_clean_bytes: u64,
    pub shared_dirty_bytes: u64,
    /// `VmSize` — address space. Large and mostly meaningless under a 2 GiB
    /// guard region per slot (§8.2.1); recorded so nobody mistakes it for a
    /// memory cost later.
    pub vsize_bytes: u64,
}

impl MemSample {
    /// Sample now. Missing fields read as zero rather than failing the whole
    /// sample: `smaps_rollup` needs kernel ≥ 4.14 and is occasionally absent
    /// in restricted containers, and a partial sample with `rss` is still
    /// worth having as long as the report says which fields are live.
    pub fn now() -> io::Result<Self> {
        let mut s = MemSample::default();
        let status = fs::read_to_string("/proc/self/status")?;
        for line in status.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let kb = parse_kb(v);
            match k {
                "VmRSS" => s.rss_bytes = kb,
                "VmHWM" => s.peak_rss_bytes = kb,
                "VmSize" => s.vsize_bytes = kb,
                _ => {}
            }
        }
        if let Ok(rollup) = fs::read_to_string("/proc/self/smaps_rollup") {
            for line in rollup.lines() {
                let Some((k, v)) = line.split_once(':') else { continue };
                let kb = parse_kb(v);
                match k {
                    "Pss" => s.pss_bytes = kb,
                    "Private_Dirty" => s.private_dirty_bytes = kb,
                    "Private_Clean" => s.private_clean_bytes = kb,
                    "Shared_Clean" => s.shared_clean_bytes = kb,
                    "Shared_Dirty" => s.shared_dirty_bytes = kb,
                    _ => {}
                }
            }
        }
        Ok(s)
    }

    /// Whether `smaps_rollup` was readable. The C1 verdict depends on it, so a
    /// scenario that cannot read it must report a GAP rather than fall back to
    /// `VmRSS` and pretend the fallback answers the same question.
    pub fn has_rollup(&self) -> bool {
        self.pss_bytes > 0 || self.private_dirty_bytes > 0
    }

    pub fn delta(&self, base: &MemSample) -> MemDelta {
        MemDelta {
            rss_bytes: self.rss_bytes as i64 - base.rss_bytes as i64,
            pss_bytes: self.pss_bytes as i64 - base.pss_bytes as i64,
            private_dirty_bytes: self.private_dirty_bytes as i64
                - base.private_dirty_bytes as i64,
            shared_clean_bytes: self.shared_clean_bytes as i64 - base.shared_clean_bytes as i64,
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct MemDelta {
    pub rss_bytes: i64,
    pub pss_bytes: i64,
    pub private_dirty_bytes: i64,
    pub shared_clean_bytes: i64,
}

fn parse_kb(v: &str) -> u64 {
    v.split_whitespace()
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

/// Total machine memory, for turning "N instances fit" into "N instances per
/// commodity server", which is the form G1 states the claim in.
pub fn machine_total_bytes() -> Option<u64> {
    let s = fs::read_to_string("/proc/meminfo").ok()?;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("MemTotal:") {
            return Some(parse_kb(v));
        }
    }
    None
}

/// A density curve: RSS and private-dirty at each step of instance count.
///
/// Kept as a series rather than a pair of endpoints because the *shape* is the
/// evidence. A straight line through the origin means every instance costs a
/// full template; a line with a positive intercept and a shallow slope is CoW
/// working, and the slope is the marginal cost the spec targets.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct DensityCurve {
    pub points: Vec<(u64, MemSample)>,
}

impl DensityCurve {
    pub fn record(&mut self, instances: u64, sample: MemSample) {
        self.points.push((instances, sample));
    }

    /// Points after the first activation.
    ///
    /// The first activation of a template in a process is not representative
    /// and never will be: it materialises the engine's pooling-allocator
    /// arena, the AOT code mapping and wasmtime's per-module state, all of
    /// which are paid once and shared by every subsequent instance. Measured
    /// on a real run it was **158 MiB** for the first instance against
    /// ~1 MiB for each of the next 199.
    ///
    /// Including it in the fit would inflate the reported marginal cost, which
    /// is the wrong error to make in *either* direction: it would understate
    /// the platform's density here, and in a different curve shape it could
    /// hide a real per-instance cost inside a large intercept. So it is
    /// excluded from the slope and reported on its own by
    /// [`DensityCurve::first_activation_bytes`].
    fn steady_state(&self) -> &[(u64, MemSample)] {
        // Keep everything from the first non-zero instance count onward, and
        // only skip the first activation when there is enough curve left to
        // fit a line through.
        let start = self.points.iter().position(|(n, _)| *n > 0).unwrap_or(0);
        let skip_first = if self.points.len() - start >= 4 { start + 1 } else { start };
        &self.points[skip_first.min(self.points.len())..]
    }

    /// Marginal bytes per instance, by least squares over the steady-state
    /// part of the curve. `None` until there are two points to fit.
    pub fn marginal_private_dirty(&self) -> Option<f64> {
        let pts: Vec<(f64, f64)> = self
            .steady_state()
            .iter()
            .map(|(n, m)| (*n as f64, m.private_dirty_bytes as f64))
            .collect();
        crate::stats::linear_fit(&pts).map(|(slope, _)| slope)
    }

    pub fn marginal_rss(&self) -> Option<f64> {
        let pts: Vec<(f64, f64)> =
            self.steady_state().iter().map(|(n, m)| (*n as f64, m.rss_bytes as f64)).collect();
        crate::stats::linear_fit(&pts).map(|(slope, _)| slope)
    }

    /// The intercept of the steady-state RSS fit: what the cell costs before
    /// any *marginal* instance. Reported separately so that fixed overhead is
    /// never smeared across the per-instance number.
    pub fn fixed_overhead_bytes(&self) -> Option<f64> {
        let pts: Vec<(f64, f64)> =
            self.steady_state().iter().map(|(n, m)| (*n as f64, m.rss_bytes as f64)).collect();
        crate::stats::linear_fit(&pts).map(|(_, intercept)| intercept)
    }

    /// RSS growth from zero instances to one: the one-time cost of the first
    /// activation of a template in this process. Paid once, shared by every
    /// instance after it, and therefore not part of the marginal cost §6.2
    /// makes its claim about.
    pub fn first_activation_bytes(&self) -> Option<i64> {
        let zero = self.points.iter().find(|(n, _)| *n == 0)?;
        let one = self.points.iter().find(|(n, _)| *n == 1)?;
        Some(one.1.rss_bytes as i64 - zero.1.rss_bytes as i64)
    }

    /// §6.2's "fixed bookkeeping ≤ 64 KB" target: marginal cost *above* what
    /// the guest itself dirtied. If each instance is asked to dirty
    /// `guest_dirty_bytes` and the marginal cost is more than that, the excess
    /// is the platform's per-instance bookkeeping.
    pub fn bookkeeping_per_instance(&self, guest_dirty_bytes: u64) -> Option<f64> {
        self.marginal_private_dirty().map(|m| m - guest_dirty_bytes as f64)
    }

    /// Marginal PSS per instance.
    ///
    /// PSS is the number a capacity planner should size from: it divides each
    /// shared page by the number of mappers, so a template page mapped by
    /// 5,000 instances is charged 1/5000 to each rather than once to the
    /// process (RSS) or not at all. `docs/spike-findings.md` measured the two
    /// diverging by ~64 kB/instance at N=2000 — the kernel's fault-around
    /// pulling a 64 kB region of shared template in whenever an instance reads
    /// a page it never wrote — so quoting RSS where PSS was meant overstates
    /// marginal cost, and quoting it the other way understates it.
    pub fn marginal_pss(&self) -> Option<f64> {
        let pts: Vec<(f64, f64)> =
            self.steady_state().iter().map(|(n, m)| (*n as f64, m.pss_bytes as f64)).collect();
        crate::stats::linear_fit(&pts).map(|(slope, _)| slope)
    }

    /// How well the steady-state private-dirty points lie on a line. A density
    /// slope quoted without this is a slope with no error bar.
    pub fn fit_quality(&self) -> Option<f64> {
        let pts: Vec<(f64, f64)> = self
            .steady_state()
            .iter()
            .map(|(n, m)| (*n as f64, m.private_dirty_bytes as f64))
            .collect();
        crate::stats::r_squared(&pts)
    }

    /// How many points the slope was actually fitted through.
    ///
    /// Not the same as `points.len()`: the zero point and the first activation
    /// are excluded from the fit (see [`DensityCurve::steady_state`]), and a
    /// report that prints the wrong one invites a reader to check the
    /// arithmetic and find it does not reproduce.
    pub fn fitted_points(&self) -> usize {
        self.steady_state().len()
    }
}

/// Human bytes, for report lines.
pub fn human(bytes: f64) -> String {
    let neg = if bytes < 0.0 { "-" } else { "" };
    let b = bytes.abs();
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{neg}{v:.2} {}", UNITS[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_this_process_reports_something_resident() {
        let s = MemSample::now().expect("/proc/self/status must be readable");
        assert!(s.rss_bytes > 0, "a running test process has resident memory");
        assert!(s.vsize_bytes >= s.rss_bytes);
    }

    /// Dirtying a megabyte must show up in `Private_Dirty`, or the C1
    /// measurement instrument is broken and every density number downstream of
    /// it is worthless. This test is about the *instrument*, not the platform.
    #[test]
    fn private_dirty_tracks_pages_we_actually_write() {
        let before = MemSample::now().unwrap();
        if !before.has_rollup() {
            eprintln!("smaps_rollup unavailable; skipping instrument check");
            return;
        }
        let mut v: Vec<u8> = Vec::with_capacity(8 << 20);
        for i in 0..(8 << 20) {
            v.push((i % 251) as u8);
        }
        std::hint::black_box(&v);
        let after = MemSample::now().unwrap();
        let d = after.delta(&before);
        assert!(
            d.private_dirty_bytes > 6 << 20,
            "wrote 8 MiB, Private_Dirty moved by only {}",
            human(d.private_dirty_bytes as f64)
        );
    }

    #[test]
    fn a_curve_separates_marginal_cost_from_fixed_overhead() {
        let mut c = DensityCurve::default();
        for n in 1..=5u64 {
            c.record(
                n,
                MemSample {
                    rss_bytes: 100_000_000 + n * 1_048_576,
                    private_dirty_bytes: 50_000_000 + n * 1_048_576,
                    ..Default::default()
                },
            );
        }
        let marginal = c.marginal_private_dirty().unwrap();
        assert!((marginal - 1_048_576.0).abs() < 1.0, "marginal was {marginal}");
        let fixed = c.fixed_overhead_bytes().unwrap();
        assert!((fixed - 100_000_000.0).abs() < 2.0, "fixed was {fixed}");
        // A guest that dirties exactly 1 MiB leaves ~0 bookkeeping.
        assert!(c.bookkeeping_per_instance(1 << 20).unwrap().abs() < 1.0);
    }

    /// The correction that a real run forced. The first activation of a
    /// template materialises engine-wide state — measured at 158 MiB against
    /// ~1 MiB per subsequent instance — and letting that into the fit would
    /// report a marginal cost many times the truth.
    #[test]
    fn the_first_activations_one_time_cost_stays_out_of_the_slope() {
        let mut c = DensityCurve::default();
        c.record(0, MemSample { rss_bytes: 18 << 20, private_dirty_bytes: 16 << 20, ..Default::default() });
        // The first instance pays 158 MiB of one-time engine state.
        c.record(1, MemSample { rss_bytes: 176 << 20, private_dirty_bytes: 174 << 20, ..Default::default() });
        for n in [2u64, 4, 8, 16, 32, 64, 128, 200] {
            c.record(
                n,
                MemSample {
                    rss_bytes: (176 << 20) + (n - 1) * 1_048_576,
                    private_dirty_bytes: (174 << 20) + (n - 1) * 1_048_576,
                    ..Default::default()
                },
            );
        }
        let marginal = c.marginal_rss().unwrap();
        assert!(
            (marginal - 1_048_576.0).abs() < 4096.0,
            "marginal should be ~1 MiB, got {}",
            human(marginal)
        );
        assert_eq!(c.first_activation_bytes(), Some(158 << 20));
    }

    /// With too few points to spare one, nothing is skipped — a short curve
    /// should still produce a number, clearly labelled, rather than nothing.
    #[test]
    fn a_short_curve_keeps_every_point() {
        let mut c = DensityCurve::default();
        c.record(0, MemSample { rss_bytes: 100, ..Default::default() });
        c.record(1, MemSample { rss_bytes: 200, ..Default::default() });
        c.record(2, MemSample { rss_bytes: 300, ..Default::default() });
        assert_eq!(c.marginal_rss(), Some(100.0));
    }

    #[test]
    fn human_is_readable() {
        assert_eq!(human(1024.0), "1.00 KiB");
        assert_eq!(human(-1536.0), "-1.50 KiB");
    }
}
