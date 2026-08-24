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

    /// Marginal bytes per instance, by least squares over the whole curve.
    /// `None` until there are two points to fit.
    pub fn marginal_private_dirty(&self) -> Option<f64> {
        let pts: Vec<(f64, f64)> = self
            .points
            .iter()
            .map(|(n, m)| (*n as f64, m.private_dirty_bytes as f64))
            .collect();
        crate::stats::linear_fit(&pts).map(|(slope, _)| slope)
    }

    pub fn marginal_rss(&self) -> Option<f64> {
        let pts: Vec<(f64, f64)> =
            self.points.iter().map(|(n, m)| (*n as f64, m.rss_bytes as f64)).collect();
        crate::stats::linear_fit(&pts).map(|(slope, _)| slope)
    }

    /// The intercept of the RSS fit: what the cell costs with zero instances
    /// resident. Reported separately so that fixed overhead is never smeared
    /// across the per-instance number.
    pub fn fixed_overhead_bytes(&self) -> Option<f64> {
        let pts: Vec<(f64, f64)> =
            self.points.iter().map(|(n, m)| (*n as f64, m.rss_bytes as f64)).collect();
        crate::stats::linear_fit(&pts).map(|(_, intercept)| intercept)
    }

    /// §6.2's "fixed bookkeeping ≤ 64 KB" target: marginal cost *above* what
    /// the guest itself dirtied. If each instance is asked to dirty
    /// `guest_dirty_bytes` and the marginal cost is more than that, the excess
    /// is the platform's per-instance bookkeeping.
    pub fn bookkeeping_per_instance(&self, guest_dirty_bytes: u64) -> Option<f64> {
        self.marginal_private_dirty().map(|m| m - guest_dirty_bytes as f64)
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

    #[test]
    fn human_is_readable() {
        assert_eq!(human(1024.0), "1.00 KiB");
        assert_eq!(human(-1536.0), "-1.50 KiB");
    }
}
