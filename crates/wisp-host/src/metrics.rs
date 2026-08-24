//! Cell metrics (spec.md §10).
//!
//! > **Metrics (per app, exported + dashboard):** dispatches, CPU-ms, p50/p99
//! > activation & wake, live/parked/hibernated instance counts, dirty-set size
//! > distribution, delta storage bytes, GC time share, trap rate.
//!
//! The point of §10 is stated a line earlier and is worth keeping in view:
//! *"the lifecycle is visible, so 'why was this request 12 ms' decomposes into
//! '9 ms hibernation wake' rather than mystery."* So the histograms here are
//! per lifecycle transition, not one blended request timer.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// A bounded-sample latency distribution. Exact percentiles over the retained
/// window rather than approximations over all time — the window is what an
/// operator is looking at anyway.
#[derive(Debug)]
pub struct Histogram {
    samples: Mutex<Vec<u64>>,
    cap: usize,
    count: AtomicU64,
    sum: AtomicU64,
}

impl Histogram {
    pub fn new(cap: usize) -> Self {
        Self { samples: Mutex::new(Vec::with_capacity(cap)), cap, count: AtomicU64::new(0), sum: AtomicU64::new(0) }
    }
    pub fn record(&self, v: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(v, Ordering::Relaxed);
        let mut s = self.samples.lock();
        if s.len() < self.cap {
            s.push(v);
        } else {
            // Reservoir-ish: overwrite a rotating slot so the window slides
            // instead of freezing at startup.
            let i = (self.count.load(Ordering::Relaxed) as usize) % self.cap;
            s[i] = v;
        }
    }
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
    pub fn percentile(&self, p: f64) -> u64 {
        let mut s = self.samples.lock().clone();
        if s.is_empty() {
            return 0;
        }
        s.sort_unstable();
        // Nearest-rank, lower-inclusive: p50 of 1..=100 is 50, not 51. Using
        // `round` here would make the median of an even sample the upper of
        // the two middles, which reads as a bug every time someone checks it
        // by hand.
        let idx = ((s.len() as f64 - 1.0) * p).floor() as usize;
        s[idx.min(s.len() - 1)]
    }
    pub fn p50(&self) -> u64 {
        self.percentile(0.50)
    }
    pub fn p99(&self) -> u64 {
        self.percentile(0.99)
    }
    pub fn mean(&self) -> f64 {
        let c = self.count();
        if c == 0 {
            0.0
        } else {
            self.sum.load(Ordering::Relaxed) as f64 / c as f64
        }
    }
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new(4096)
    }
}

/// Everything §10 asks a cell to expose.
#[derive(Debug, Default)]
pub struct Metrics {
    pub dispatches: AtomicU64,
    pub wakes: AtomicU64,
    pub traps: AtomicU64,
    pub budget_kills: AtomicU64,
    pub shed: AtomicU64,
    pub breaker_rejections: AtomicU64,
    pub reentrancy_refusals: AtomicU64,
    pub pending_cap_rejections: AtomicU64,
    pub egress_denials: AtomicU64,

    pub live: AtomicU64,
    pub parked: AtomicU64,
    pub hibernated: AtomicU64,
    pub hot: AtomicU64,

    pub captures: AtomicU64,
    pub delta_bytes: AtomicU64,
    pub hibernation_wakes: AtomicU64,
    pub delta_fallbacks: AtomicU64,
    pub futures_shed: AtomicU64,
    pub futures_buffered: AtomicU64,

    /// §6.5's target latencies. Microseconds.
    pub activation_us: Histogram,
    pub wake_us: Histogram,
    pub hibernation_wake_us: Histogram,
    pub capture_us: Histogram,
    /// End-to-end request latency, admission to final response — the thing
    /// §10 wants to decompose into "9 ms hibernation wake" rather than leave
    /// as mystery.
    pub request_us: Histogram,
    pub park_ms: Histogram,
    /// §6.6 telemetry: "your instance dirties 60 MB".
    pub dirty_pages: Histogram,
    /// §10 GC time share — the `wisp-gc-hint(snapshot)` call before capture.
    pub gc_hint_us: Histogram,

    /// §9.2 `tier=fast` visibility: how many dispatches were served without
    /// paying for an activation.
    pub served_warm: AtomicU64,
    pub served_pooled_fresh: AtomicU64,
    pub served_activated: AtomicU64,

    by_tenant: Mutex<BTreeMap<String, TenantCounters>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TenantCounters {
    pub dispatches: u64,
    pub cpu_ns: u64,
    pub traps: u64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_tenant(&self, tenant: &str, cpu_ns: u64, trapped: bool) {
        let mut g = self.by_tenant.lock();
        let e = g.entry(tenant.to_string()).or_default();
        e.dispatches += 1;
        e.cpu_ns += cpu_ns;
        if trapped {
            e.traps += 1;
        }
    }

    pub fn tenant(&self, tenant: &str) -> TenantCounters {
        self.by_tenant.lock().get(tenant).copied().unwrap_or_default()
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        MetricsSnapshot {
            dispatches: l(&self.dispatches),
            wakes: l(&self.wakes),
            traps: l(&self.traps),
            budget_kills: l(&self.budget_kills),
            shed: l(&self.shed),
            breaker_rejections: l(&self.breaker_rejections),
            reentrancy_refusals: l(&self.reentrancy_refusals),
            pending_cap_rejections: l(&self.pending_cap_rejections),
            egress_denials: l(&self.egress_denials),
            live: l(&self.live),
            parked: l(&self.parked),
            hibernated: l(&self.hibernated),
            hot: l(&self.hot),
            captures: l(&self.captures),
            delta_bytes: l(&self.delta_bytes),
            hibernation_wakes: l(&self.hibernation_wakes),
            delta_fallbacks: l(&self.delta_fallbacks),
            futures_shed: l(&self.futures_shed),
            futures_buffered: l(&self.futures_buffered),
            served_warm: l(&self.served_warm),
            served_pooled_fresh: l(&self.served_pooled_fresh),
            served_activated: l(&self.served_activated),
            activation_p50_us: self.activation_us.p50(),
            activation_p99_us: self.activation_us.p99(),
            wake_p50_us: self.wake_us.p50(),
            wake_p99_us: self.wake_us.p99(),
            hibernation_wake_p50_us: self.hibernation_wake_us.p50(),
            hibernation_wake_p99_us: self.hibernation_wake_us.p99(),
            capture_p50_us: self.capture_us.p50(),
            request_p50_us: self.request_us.p50(),
            request_p99_us: self.request_us.p99(),
            gc_hint_p50_us: self.gc_hint_us.p50(),
            dirty_pages_p50: self.dirty_pages.p50(),
            dirty_pages_p99: self.dirty_pages.p99(),
            park_p50_ms: self.park_ms.p50(),
            trap_rate: {
                let d = l(&self.dispatches);
                if d == 0 { 0.0 } else { l(&self.traps) as f64 / d as f64 }
            },
            tenants: self.by_tenant.lock().clone(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.snapshot().as_json()).unwrap_or_else(|_| "{}".into())
    }

    /// Prometheus-ish text exposition, so the harness can scrape without a
    /// client library.
    pub fn to_text(&self) -> String {
        let s = self.snapshot();
        let mut out = String::new();
        for (k, v) in s.as_pairs() {
            out.push_str(&format!("wisp_cell_{k} {v}\n"));
        }
        for (tenant, c) in &s.tenants {
            out.push_str(&format!("wisp_tenant_dispatches{{tenant=\"{tenant}\"}} {}\n", c.dispatches));
            out.push_str(&format!(
                "wisp_tenant_cpu_ms{{tenant=\"{tenant}\"}} {:.3}\n",
                c.cpu_ns as f64 / 1e6
            ));
            out.push_str(&format!("wisp_tenant_traps{{tenant=\"{tenant}\"}} {}\n", c.traps));
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub dispatches: u64,
    pub wakes: u64,
    pub traps: u64,
    pub budget_kills: u64,
    pub shed: u64,
    pub breaker_rejections: u64,
    pub reentrancy_refusals: u64,
    pub pending_cap_rejections: u64,
    pub egress_denials: u64,
    pub live: u64,
    pub parked: u64,
    pub hibernated: u64,
    pub hot: u64,
    pub captures: u64,
    pub delta_bytes: u64,
    pub hibernation_wakes: u64,
    pub delta_fallbacks: u64,
    pub futures_shed: u64,
    pub futures_buffered: u64,
    pub served_warm: u64,
    pub served_pooled_fresh: u64,
    pub served_activated: u64,
    pub activation_p50_us: u64,
    pub activation_p99_us: u64,
    pub wake_p50_us: u64,
    pub wake_p99_us: u64,
    pub hibernation_wake_p50_us: u64,
    pub hibernation_wake_p99_us: u64,
    pub capture_p50_us: u64,
    pub request_p50_us: u64,
    pub request_p99_us: u64,
    pub gc_hint_p50_us: u64,
    pub dirty_pages_p50: u64,
    pub dirty_pages_p99: u64,
    pub park_p50_ms: u64,
    pub trap_rate: f64,
    pub tenants: BTreeMap<String, TenantCounters>,
}

impl MetricsSnapshot {
    pub fn as_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("dispatches", self.dispatches.to_string()),
            ("wakes", self.wakes.to_string()),
            ("traps", self.traps.to_string()),
            ("budget_kills", self.budget_kills.to_string()),
            ("shed", self.shed.to_string()),
            ("breaker_rejections", self.breaker_rejections.to_string()),
            ("reentrancy_refusals", self.reentrancy_refusals.to_string()),
            ("pending_cap_rejections", self.pending_cap_rejections.to_string()),
            ("egress_denials", self.egress_denials.to_string()),
            ("instances_live", self.live.to_string()),
            ("instances_parked", self.parked.to_string()),
            ("instances_hibernated", self.hibernated.to_string()),
            ("instances_hot", self.hot.to_string()),
            ("captures", self.captures.to_string()),
            ("delta_bytes", self.delta_bytes.to_string()),
            ("hibernation_wakes", self.hibernation_wakes.to_string()),
            ("delta_fallbacks", self.delta_fallbacks.to_string()),
            ("futures_shed", self.futures_shed.to_string()),
            ("futures_buffered", self.futures_buffered.to_string()),
            ("served_warm", self.served_warm.to_string()),
            ("served_pooled_fresh", self.served_pooled_fresh.to_string()),
            ("served_activated", self.served_activated.to_string()),
            ("activation_p50_us", self.activation_p50_us.to_string()),
            ("activation_p99_us", self.activation_p99_us.to_string()),
            ("wake_p50_us", self.wake_p50_us.to_string()),
            ("wake_p99_us", self.wake_p99_us.to_string()),
            ("hibernation_wake_p50_us", self.hibernation_wake_p50_us.to_string()),
            ("hibernation_wake_p99_us", self.hibernation_wake_p99_us.to_string()),
            ("capture_p50_us", self.capture_p50_us.to_string()),
            ("request_p50_us", self.request_p50_us.to_string()),
            ("request_p99_us", self.request_p99_us.to_string()),
            ("gc_hint_p50_us", self.gc_hint_p50_us.to_string()),
            ("dirty_pages_p50", self.dirty_pages_p50.to_string()),
            ("dirty_pages_p99", self.dirty_pages_p99.to_string()),
            ("park_p50_ms", self.park_p50_ms.to_string()),
            ("trap_rate", format!("{:.6}", self.trap_rate)),
        ]
    }

    pub fn as_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for (k, v) in self.as_pairs() {
            let val: serde_json::Value = v
                .parse::<u64>()
                .map(Into::into)
                .or_else(|_| v.parse::<f64>().map(Into::into))
                .unwrap_or(serde_json::Value::String(v));
            m.insert(k.to_string(), val);
        }
        let tenants: serde_json::Map<String, serde_json::Value> = self
            .tenants
            .iter()
            .map(|(t, c)| {
                (
                    t.clone(),
                    serde_json::json!({
                        "dispatches": c.dispatches,
                        "cpu_ms": c.cpu_ns as f64 / 1e6,
                        "traps": c.traps,
                    }),
                )
            })
            .collect();
        m.insert("tenants".into(), serde_json::Value::Object(tenants));
        serde_json::Value::Object(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_exact_over_the_window() {
        let h = Histogram::new(1000);
        for i in 1..=100u64 {
            h.record(i);
        }
        assert_eq!(h.count(), 100);
        assert_eq!(h.p50(), 50);
        assert_eq!(h.p99(), 99);
        assert!((h.mean() - 50.5).abs() < 0.001);
    }

    #[test]
    fn an_empty_histogram_reads_zero_rather_than_panicking() {
        let h = Histogram::default();
        assert_eq!(h.p50(), 0);
        assert_eq!(h.p99(), 0);
        assert_eq!(h.mean(), 0.0);
    }

    /// §10's list, present and scrapeable. The harness reads this endpoint, so
    /// a rename is a breaking change to it.
    #[test]
    fn the_scrape_carries_every_metric_section_10_names() {
        let m = Metrics::new();
        m.dispatches.fetch_add(7, Ordering::Relaxed);
        m.traps.fetch_add(1, Ordering::Relaxed);
        m.activation_us.record(900);
        m.wake_us.record(40);
        m.dirty_pages.record(512);
        m.delta_bytes.fetch_add(4096, Ordering::Relaxed);
        m.record_tenant("acme", 2_000_000, false);

        let text = m.to_text();
        for needle in [
            "wisp_cell_dispatches 7",
            "wisp_cell_activation_p50_us 900",
            "wisp_cell_wake_p50_us 40",
            "wisp_cell_dirty_pages_p50 512",
            "wisp_cell_delta_bytes 4096",
            "wisp_cell_instances_parked",
            "wisp_cell_instances_hibernated",
            "wisp_cell_trap_rate",
            "wisp_tenant_cpu_ms{tenant=\"acme\"} 2.000",
        ] {
            assert!(text.contains(needle), "scrape is missing `{needle}`:\n{text}");
        }

        let json: serde_json::Value = serde_json::from_str(&m.to_json()).unwrap();
        assert_eq!(json["dispatches"], 7);
        assert_eq!(json["tenants"]["acme"]["dispatches"], 1);
        assert!((json["trap_rate"].as_f64().unwrap() - 1.0 / 7.0).abs() < 1e-6);
    }
}
