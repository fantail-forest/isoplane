//! What one cell exposes to a scrape. The histograms are per lifecycle
//! transition rather than one blended dispatch timer, so a slow dispatch
//! decomposes into a named transition.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// A bounded-sample latency distribution: exact percentiles over the retained
/// window rather than approximations over all time.
#[derive(Debug)]
pub struct Histogram {
    samples: Mutex<Vec<u64>>,
    cap: usize,
    count: AtomicU64,
    sum: AtomicU64,
}

impl Histogram {
    pub fn new(cap: usize) -> Self {
        Self {
            samples: Mutex::new(Vec::with_capacity(cap)),
            cap,
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
        }
    }
    pub fn record(&self, v: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(v, Ordering::Relaxed);
        let mut s = self.samples.lock();
        if s.len() < self.cap {
            s.push(v);
        } else {
            // A rotating slot, so the window slides instead of freezing at
            // startup.
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
        // Nearest rank, lower inclusive: p50 of 1..=100 is 50.
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

#[derive(Debug, Default)]
pub struct Metrics {
    pub dispatches: AtomicU64,
    /// Wakes the wake router enqueued.
    pub wakes: AtomicU64,
    /// Wakes queued inside the cell, for a completion that landed while the
    /// dispatch was still Hot; not in [`Metrics::wakes`].
    pub in_cell_wakes: AtomicU64,
    /// Completions for an instance nobody could hand them to. The result is
    /// already in the future table, so for a live instance this is a dispatch
    /// that is never answered; for a discarded one it is the correct outcome.
    pub wakes_dropped: AtomicU64,
    pub traps: AtomicU64,
    /// Every occupancy-budget kill, clean and hard.
    pub budget_kills: AtomicU64,
    /// Dispatches that entered grace and returned inside it; the slot was
    /// reused.
    pub clean_kills: AtomicU64,
    /// Dispatches still executing at the end of grace, terminated without
    /// unwinding and discarded. The hang breaker counts these. Disjoint from
    /// `traps`: a hard kill is not a trap.
    pub hard_kills: AtomicU64,
    /// Dispatches that crossed the budget at all.
    pub grace_entries: AtomicU64,
    /// Epoch ticks that found the unsafe-to-trap bit set and deferred grace.
    pub grace_deferrals: AtomicU64,
    /// occupancy recorded past the budget on kills of either phase; already inside
    /// the tenant's `occupancy_ns`.
    pub overage_ns: AtomicU64,

    pub shed: AtomicU64,
    pub breaker_rejections: AtomicU64,
    /// A wake that found a previous call into its instance still running and
    /// left the result queued for it to drain.
    pub reentrancy_refusals: AtomicU64,
    pub pending_cap_rejections: AtomicU64,
    /// Egress-policy refusals plus subrequest-cap refusals.
    pub egress_denials: AtomicU64,

    pub live: AtomicU64,
    pub parked: AtomicU64,
    pub hibernated: AtomicU64,
    pub hot: AtomicU64,

    /// Captures whose delta reached the store.
    pub captures: AtomicU64,
    /// Delta bytes that reached the store.
    pub delta_bytes: AtomicU64,
    /// Captures whose store write failed; the instance stays Parked and
    /// capturable.
    pub capture_write_failures: AtomicU64,
    pub hibernation_wakes: AtomicU64,
    pub delta_fallbacks: AtomicU64,
    /// Futures shed at capture: Unbounded futures and sensitive ones. Not a
    /// load signal; it moves when hibernation works. The load-path counters
    /// are `pending_cap_rejections` and `shed`.
    pub futures_shed: AtomicU64,
    /// Equal to `futures_shed` by construction; the aggregate name stays for
    /// existing dashboards.
    pub futures_shed_capture: AtomicU64,
    pub futures_buffered: AtomicU64,

    pub activation_us: Histogram,
    pub wake_us: Histogram,
    pub hibernation_wake_us: Histogram,
    pub capture_us: Histogram,
    /// Admission to final response.
    pub dispatch_us: Histogram,
    pub park_ms: Histogram,
    pub dirty_pages: Histogram,
    /// The pre-capture gc hint.
    pub gc_hint_us: Histogram,

    /// How a dispatch got its instance, for both instance kinds. The four are
    /// exhaustive over admitted dispatches: their sum is `dispatches`.
    pub served_warm: AtomicU64,
    pub served_pooled_fresh: AtomicU64,
    pub served_hibernation_wake: AtomicU64,
    pub served_activated: AtomicU64,

    by_tenant: Mutex<BTreeMap<String, TenantCounters>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TenantCounters {
    pub dispatches: u64,
    pub occupancy_ns: u64,
    pub traps: u64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_tenant(&self, tenant: &str, occupancy_ns: u64, trapped: bool) {
        let mut g = self.by_tenant.lock();
        let e = g.entry(tenant.to_string()).or_default();
        e.dispatches += 1;
        e.occupancy_ns += occupancy_ns;
        if trapped {
            e.traps += 1;
        }
    }

    pub fn tenant(&self, tenant: &str) -> TenantCounters {
        self.by_tenant
            .lock()
            .get(tenant)
            .copied()
            .unwrap_or_default()
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        MetricsSnapshot {
            dispatches: l(&self.dispatches),
            wakes: l(&self.wakes),
            in_cell_wakes: l(&self.in_cell_wakes),
            wakes_dropped: l(&self.wakes_dropped),
            traps: l(&self.traps),
            budget_kills: l(&self.budget_kills),
            clean_kills: l(&self.clean_kills),
            hard_kills: l(&self.hard_kills),
            grace_entries: l(&self.grace_entries),
            grace_deferrals: l(&self.grace_deferrals),
            overage_ns: l(&self.overage_ns),
            // With no hard kills the ratio is reported as the clean count,
            // the most it can claim.
            clean_hard_ratio: {
                let (c, h) = (l(&self.clean_kills), l(&self.hard_kills));
                if h == 0 {
                    c as f64
                } else {
                    c as f64 / h as f64
                }
            },
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
            capture_write_failures: l(&self.capture_write_failures),
            hibernation_wakes: l(&self.hibernation_wakes),
            delta_fallbacks: l(&self.delta_fallbacks),
            futures_shed: l(&self.futures_shed),
            futures_shed_capture: l(&self.futures_shed_capture),
            futures_buffered: l(&self.futures_buffered),
            served_warm: l(&self.served_warm),
            served_pooled_fresh: l(&self.served_pooled_fresh),
            served_hibernation_wake: l(&self.served_hibernation_wake),
            served_activated: l(&self.served_activated),
            activation_p50_us: self.activation_us.p50(),
            activation_p99_us: self.activation_us.p99(),
            wake_p50_us: self.wake_us.p50(),
            wake_p99_us: self.wake_us.p99(),
            hibernation_wake_p50_us: self.hibernation_wake_us.p50(),
            hibernation_wake_p99_us: self.hibernation_wake_us.p99(),
            capture_p50_us: self.capture_us.p50(),
            dispatch_p50_us: self.dispatch_us.p50(),
            dispatch_p99_us: self.dispatch_us.p99(),
            gc_hint_p50_us: self.gc_hint_us.p50(),
            dirty_pages_p50: self.dirty_pages.p50(),
            dirty_pages_p99: self.dirty_pages.p99(),
            park_p50_ms: self.park_ms.p50(),
            // Process-wide, and both crates' clocks: the engine's is where the
            // recorded `occupancy_ns` comes from, this crate's is the log-occupancy share.
            clock_read_failures: crate::meters::clock_read_failures()
                + isoplane_engine::clock_read_failures(),
            trap_rate: {
                let d = l(&self.dispatches);
                if d == 0 {
                    0.0
                } else {
                    l(&self.traps) as f64 / d as f64
                }
            },
            tenants: self.by_tenant.lock().clone(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.snapshot().as_json()).unwrap_or_else(|_| "{}".into())
    }

    pub fn to_text(&self) -> String {
        let s = self.snapshot();
        let mut out = String::new();
        for (k, v) in s.as_pairs() {
            out.push_str(&format!("isoplane_cell_{k} {v}\n"));
        }
        for (tenant, c) in &s.tenants {
            out.push_str(&format!(
                "isoplane_tenant_dispatches{{tenant=\"{tenant}\"}} {}\n",
                c.dispatches
            ));
            out.push_str(&format!(
                "isoplane_tenant_occupancy_ms{{tenant=\"{tenant}\"}} {:.3}\n",
                c.occupancy_ns as f64 / 1e6
            ));
            out.push_str(&format!(
                "isoplane_tenant_traps{{tenant=\"{tenant}\"}} {}\n",
                c.traps
            ));
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub dispatches: u64,
    pub wakes: u64,
    pub in_cell_wakes: u64,
    pub wakes_dropped: u64,
    pub traps: u64,
    pub budget_kills: u64,
    pub clean_kills: u64,
    pub hard_kills: u64,
    pub grace_entries: u64,
    pub grace_deferrals: u64,
    pub overage_ns: u64,
    pub clean_hard_ratio: f64,
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
    pub capture_write_failures: u64,
    pub hibernation_wakes: u64,
    pub delta_fallbacks: u64,
    pub futures_shed: u64,
    pub futures_shed_capture: u64,
    pub futures_buffered: u64,
    pub served_warm: u64,
    pub served_pooled_fresh: u64,
    pub served_hibernation_wake: u64,
    pub served_activated: u64,
    pub activation_p50_us: u64,
    pub activation_p99_us: u64,
    pub wake_p50_us: u64,
    pub wake_p99_us: u64,
    pub hibernation_wake_p50_us: u64,
    pub hibernation_wake_p99_us: u64,
    pub capture_p50_us: u64,
    pub dispatch_p50_us: u64,
    pub dispatch_p99_us: u64,
    pub gc_hint_p50_us: u64,
    pub dirty_pages_p50: u64,
    pub dirty_pages_p99: u64,
    pub park_p50_ms: u64,
    pub clock_read_failures: u64,
    pub trap_rate: f64,
    pub tenants: BTreeMap<String, TenantCounters>,
}

impl MetricsSnapshot {
    pub fn as_pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("dispatches", self.dispatches.to_string()),
            ("wakes", self.wakes.to_string()),
            ("in_cell_wakes", self.in_cell_wakes.to_string()),
            ("wakes_dropped", self.wakes_dropped.to_string()),
            ("traps", self.traps.to_string()),
            ("budget_kills", self.budget_kills.to_string()),
            ("clean_kills", self.clean_kills.to_string()),
            ("hard_kills", self.hard_kills.to_string()),
            ("grace_entries", self.grace_entries.to_string()),
            ("grace_deferrals", self.grace_deferrals.to_string()),
            ("overage_ms", format!("{:.3}", self.overage_ns as f64 / 1e6)),
            ("clean_hard_ratio", format!("{:.6}", self.clean_hard_ratio)),
            ("shed", self.shed.to_string()),
            ("breaker_rejections", self.breaker_rejections.to_string()),
            ("reentrancy_refusals", self.reentrancy_refusals.to_string()),
            (
                "pending_cap_rejections",
                self.pending_cap_rejections.to_string(),
            ),
            ("egress_denials", self.egress_denials.to_string()),
            ("instances_live", self.live.to_string()),
            ("instances_parked", self.parked.to_string()),
            ("instances_hibernated", self.hibernated.to_string()),
            ("instances_hot", self.hot.to_string()),
            ("captures", self.captures.to_string()),
            ("delta_bytes", self.delta_bytes.to_string()),
            (
                "capture_write_failures",
                self.capture_write_failures.to_string(),
            ),
            ("hibernation_wakes", self.hibernation_wakes.to_string()),
            ("delta_fallbacks", self.delta_fallbacks.to_string()),
            ("futures_shed", self.futures_shed.to_string()),
            (
                "futures_shed_capture",
                self.futures_shed_capture.to_string(),
            ),
            ("futures_buffered", self.futures_buffered.to_string()),
            ("served_warm", self.served_warm.to_string()),
            ("served_pooled_fresh", self.served_pooled_fresh.to_string()),
            (
                "served_hibernation_wake",
                self.served_hibernation_wake.to_string(),
            ),
            ("served_activated", self.served_activated.to_string()),
            ("activation_p50_us", self.activation_p50_us.to_string()),
            ("activation_p99_us", self.activation_p99_us.to_string()),
            ("wake_p50_us", self.wake_p50_us.to_string()),
            ("wake_p99_us", self.wake_p99_us.to_string()),
            (
                "hibernation_wake_p50_us",
                self.hibernation_wake_p50_us.to_string(),
            ),
            (
                "hibernation_wake_p99_us",
                self.hibernation_wake_p99_us.to_string(),
            ),
            ("capture_p50_us", self.capture_p50_us.to_string()),
            ("dispatch_p50_us", self.dispatch_p50_us.to_string()),
            ("dispatch_p99_us", self.dispatch_p99_us.to_string()),
            ("gc_hint_p50_us", self.gc_hint_p50_us.to_string()),
            ("dirty_pages_p50", self.dirty_pages_p50.to_string()),
            ("dirty_pages_p99", self.dirty_pages_p99.to_string()),
            ("park_p50_ms", self.park_p50_ms.to_string()),
            ("clock_read_failures", self.clock_read_failures.to_string()),
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
                        "occupancy_ms": c.occupancy_ns as f64 / 1e6,
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
        assert_eq!(h.count(), 100, "every sample is counted");
        assert_eq!(h.p50(), 50, "nearest rank, lower inclusive");
        assert_eq!(h.p99(), 99, "p99 of 1..=100");
        assert!((h.mean() - 50.5).abs() < 0.001, "the mean of 1..=100");
    }

    #[test]
    fn an_empty_histogram_reads_zero_rather_than_panicking() {
        let h = Histogram::default();
        assert_eq!(h.p50(), 0, "no samples, no p50");
        assert_eq!(h.p99(), 0, "no samples, no p99");
        assert_eq!(h.mean(), 0.0, "no samples, no mean");
    }

    #[test]
    fn the_clean_hard_ratio_is_a_scraped_metric() {
        let m = Metrics::new();
        for _ in 0..3_000 {
            m.clean_kills.fetch_add(1, Ordering::Relaxed);
        }
        m.hard_kills.fetch_add(1, Ordering::Relaxed);
        let s = m.snapshot();
        assert_eq!(
            s.clean_hard_ratio, 3_000.0,
            "3,000 clean against 1 hard is 3,000"
        );
        assert!(
            m.to_text()
                .contains("isoplane_cell_clean_hard_ratio 3000.000000"),
            "the ratio is on the text scrape"
        );
        let json: serde_json::Value = serde_json::from_str(&m.to_json()).expect("json");
        assert_eq!(
            json["clean_kills"], 3_000,
            "the clean count is on the json scrape"
        );
        assert_eq!(json["hard_kills"], 1, "so is the hard count");
    }

    #[test]
    fn the_clean_hard_ratio_can_report_an_unhealthy_fleet() {
        let m = Metrics::new();
        m.clean_kills.fetch_add(1, Ordering::Relaxed);
        for _ in 0..1_000 {
            m.hard_kills.fetch_add(1, Ordering::Relaxed);
        }
        assert!(
            m.snapshot().clean_hard_ratio < 0.01,
            "a fleet that is all hard kills must read as such, not as healthy"
        );
        assert_eq!(
            Metrics::new().snapshot().clean_hard_ratio,
            0.0,
            "with no kills at all the ratio must not invent a healthy number"
        );
    }

    #[test]
    fn the_scrape_carries_every_metric_name() {
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
            "isoplane_cell_dispatches 7",
            "isoplane_cell_activation_p50_us 900",
            "isoplane_cell_wake_p50_us 40",
            "isoplane_cell_dirty_pages_p50 512",
            "isoplane_cell_delta_bytes 4096",
            "isoplane_cell_instances_parked",
            "isoplane_cell_instances_hibernated",
            "isoplane_cell_trap_rate",
            "isoplane_tenant_occupancy_ms{tenant=\"acme\"} 2.000",
        ] {
            assert!(
                text.contains(needle),
                "scrape is missing `{needle}`:\n{text}"
            );
        }

        let json: serde_json::Value = serde_json::from_str(&m.to_json()).expect("json");
        assert_eq!(
            json["dispatches"], 7,
            "the json scrape carries the counters"
        );
        assert_eq!(
            json["tenants"]["acme"]["dispatches"], 1,
            "and the per-tenant counters"
        );
        assert!(
            (json["trap_rate"].as_f64().expect("a number") - 1.0 / 7.0).abs() < 1e-6,
            "the trap rate is traps over dispatches"
        );
    }
}
