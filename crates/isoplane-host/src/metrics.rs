//! What one cell exposes to a scrape. The histograms are per lifecycle
//! transition rather than one blended dispatch timer, so a slow dispatch
//! decomposes into a named transition.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use prometheus::{proto, TextEncoder};

/// Exact percentiles over the most recent 4096 observations, with lifetime
/// count and mean. Recording stops if the lifetime count reaches `u64::MAX`.
#[derive(Debug)]
pub struct Histogram {
    state: Mutex<HistogramState>,
    cap: usize,
}

#[derive(Debug, Default)]
struct HistogramState {
    samples: VecDeque<u64>,
    count: u64,
    sum: u128,
}

impl Histogram {
    fn new(cap: usize) -> Self {
        Self {
            state: Mutex::new(HistogramState {
                samples: VecDeque::with_capacity(cap),
                ..HistogramState::default()
            }),
            cap,
        }
    }

    /// Records one observation in this field's units.
    pub fn record(&self, value: u64) {
        let mut state = self.state.lock();
        let Some(count) = state.count.checked_add(1) else {
            tracing::warn!("host histogram lifetime count exhausted; observation dropped");
            return;
        };
        if self.cap != 0 {
            if state.samples.len() == self.cap {
                state.samples.pop_front();
            }
            state.samples.push_back(value);
        }
        state.count = count;
        state.sum += u128::from(value);
    }

    /// Returns the number of accepted lifetime observations.
    pub fn count(&self) -> u64 {
        self.state.lock().count
    }

    /// Returns the sum of accepted lifetime observations in this field's units.
    pub fn sum(&self) -> u128 {
        self.state.lock().sum
    }

    fn percentile(&self, percentile: f64) -> u64 {
        let mut samples: Vec<_> = self.state.lock().samples.iter().copied().collect();
        if samples.is_empty() {
            return 0;
        }
        samples.sort_unstable();
        let index = ((samples.len() as f64 - 1.0) * percentile).floor() as usize;
        samples[index.min(samples.len() - 1)]
    }

    /// Returns the lower-inclusive median of the retained window, or zero if empty.
    pub fn p50(&self) -> u64 {
        self.percentile(0.50)
    }

    /// Returns the lower-inclusive 99th percentile of the window, or zero if empty.
    pub fn p99(&self) -> u64 {
        self.percentile(0.99)
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
    pub traps: AtomicU64,
    /// Times an instance stopped taking new dispatches because one of its
    /// dispatches entered the grace after its cancellation.
    pub instance_holds: AtomicU64,

    pub breaker_rejections: AtomicU64,
    pub pending_cap_rejections: AtomicU64,

    pub parked: AtomicU64,
    pub hibernated: AtomicU64,
    pub hot: AtomicU64,

    /// Cumulative bytes written to local snapshot reservations.
    pub delta_bytes: AtomicU64,
    pub activation_us: Histogram,
    pub hibernation_wake_us: Histogram,
    /// Admission to final response.
    pub dispatch_us: Histogram,
    pub dirty_pages: Histogram,

    /// Dispatches that joined an instance already running another dispatch.
    pub served_shared: AtomicU64,
    /// Dispatches that got a newly activated instance.
    pub served_activated: AtomicU64,

    by_tenant: Mutex<BTreeMap<String, Arc<TenantMetrics>>>,
}

#[derive(Debug, Default)]
pub(crate) struct TenantMetrics {
    dispatches: AtomicU64,
    occupancy_ns: AtomicU64,
    traps: AtomicU64,
}

impl TenantMetrics {
    pub(crate) fn record_occupancy(&self, ns: u64) {
        self.occupancy_ns.fetch_add(ns, Ordering::Relaxed);
    }

    pub(crate) fn record_completion(&self, trapped: bool) {
        self.dispatches.fetch_add(1, Ordering::Relaxed);
        if trapped {
            self.traps.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> TenantCounters {
        TenantCounters {
            dispatches: self.dispatches.load(Ordering::Relaxed),
            occupancy_ns: self.occupancy_ns.load(Ordering::Relaxed),
            traps: self.traps.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TenantCounters {
    /// Completed dispatches, across all their slices and attributed reactor polls.
    pub dispatches: u64,
    /// Measured slice and reactor occupancy, including work after a dispatch ends.
    pub occupancy_ns: u64,
    /// Completed dispatches that ended in a guest trap, excluding budget kills.
    pub traps: u64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn tenant_accounting(&self, tenant: &str) -> Arc<TenantMetrics> {
        self.by_tenant
            .lock()
            .entry(tenant.to_owned())
            .or_default()
            .clone()
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        MetricsSnapshot {
            dispatches: l(&self.dispatches),
            traps: l(&self.traps),
            instance_holds: l(&self.instance_holds),
            breaker_rejections: l(&self.breaker_rejections),
            pending_cap_rejections: l(&self.pending_cap_rejections),
            parked: l(&self.parked),
            hibernated: l(&self.hibernated),
            hot: l(&self.hot),
            delta_bytes: l(&self.delta_bytes),
            served_shared: l(&self.served_shared),
            served_activated: l(&self.served_activated),
            activation_p50_us: self.activation_us.p50(),
            activation_p99_us: self.activation_us.p99(),
            hibernation_wake_p50_us: self.hibernation_wake_us.p50(),
            hibernation_wake_p99_us: self.hibernation_wake_us.p99(),
            dirty_pages_p50: self.dirty_pages.p50(),
            dirty_pages_p99: self.dirty_pages.p99(),
            tenants: self
                .by_tenant
                .lock()
                .iter()
                .map(|(tenant, counters)| (tenant.clone(), counters.snapshot()))
                .collect(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.snapshot().as_json()).unwrap_or_else(|_| "{}".into())
    }

    /// Encodes the current diagnostic snapshot as Prometheus gauges.
    pub fn to_text(&self) -> String {
        let snapshot = self.snapshot();
        let mut families: Vec<_> = snapshot
            .values()
            .into_iter()
            .map(|(name, value)| {
                gauge_family(&format!("isoplane_cell_{name}"), vec![(None, value as f64)])
            })
            .collect();
        if !snapshot.tenants.is_empty() {
            for (name, values) in [
                (
                    "isoplane_tenant_dispatches",
                    snapshot
                        .tenants
                        .iter()
                        .map(|(tenant, counters)| {
                            (Some(tenant.as_str()), counters.dispatches as f64)
                        })
                        .collect(),
                ),
                (
                    "isoplane_tenant_occupancy_ms",
                    snapshot
                        .tenants
                        .iter()
                        .map(|(tenant, counters)| {
                            (Some(tenant.as_str()), counters.occupancy_ns as f64 / 1e6)
                        })
                        .collect(),
                ),
                (
                    "isoplane_tenant_traps",
                    snapshot
                        .tenants
                        .iter()
                        .map(|(tenant, counters)| (Some(tenant.as_str()), counters.traps as f64))
                        .collect(),
                ),
            ] {
                families.push(gauge_family(name, values));
            }
        }
        TextEncoder::new().encode_to_string(&families).expect(
            "all families above have nonempty names and samples, and String writing cannot fail",
        )
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub dispatches: u64,
    pub traps: u64,
    pub instance_holds: u64,
    pub breaker_rejections: u64,
    pub pending_cap_rejections: u64,
    pub parked: u64,
    pub hibernated: u64,
    pub hot: u64,
    pub delta_bytes: u64,
    pub served_shared: u64,
    pub served_activated: u64,
    pub activation_p50_us: u64,
    pub activation_p99_us: u64,
    pub hibernation_wake_p50_us: u64,
    pub hibernation_wake_p99_us: u64,
    pub dirty_pages_p50: u64,
    pub dirty_pages_p99: u64,
    pub tenants: BTreeMap<String, TenantCounters>,
}

fn gauge_family(name: &str, values: Vec<(Option<&str>, f64)>) -> proto::MetricFamily {
    let mut family = proto::MetricFamily::default();
    family.set_name(name.to_owned());
    family.set_field_type(proto::MetricType::GAUGE);
    family.set_metric(
        values
            .into_iter()
            .map(|(tenant, value)| {
                let mut metric = proto::Metric::default();
                let mut gauge = proto::Gauge::default();
                gauge.set_value(value);
                metric.set_gauge(gauge);
                if let Some(tenant) = tenant {
                    let mut label = proto::LabelPair::default();
                    label.set_name("tenant".to_owned());
                    label.set_value(tenant.to_owned());
                    metric.set_label(vec![label]);
                }
                metric
            })
            .collect(),
    );
    family
}

impl MetricsSnapshot {
    fn values(&self) -> Vec<(&'static str, u64)> {
        vec![
            ("dispatches", self.dispatches),
            ("traps", self.traps),
            ("instance_holds", self.instance_holds),
            ("breaker_rejections", self.breaker_rejections),
            ("pending_cap_rejections", self.pending_cap_rejections),
            ("instances_parked", self.parked),
            ("instances_hibernated", self.hibernated),
            ("instances_hot", self.hot),
            ("delta_bytes", self.delta_bytes),
            ("served_shared", self.served_shared),
            ("served_activated", self.served_activated),
            ("activation_p50_us", self.activation_p50_us),
            ("activation_p99_us", self.activation_p99_us),
            ("hibernation_wake_p50_us", self.hibernation_wake_p50_us),
            ("hibernation_wake_p99_us", self.hibernation_wake_p99_us),
            ("dirty_pages_p50", self.dirty_pages_p50),
            ("dirty_pages_p99", self.dirty_pages_p99),
        ]
    }

    pub fn as_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for (name, value) in self.values() {
            m.insert(name.to_owned(), value.into());
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
    fn tenant_labels_cannot_inject_scrape_samples() {
        for tenant in [
            "acme",
            "quote\"",
            "back\\slash",
            "literal\\n",
            "line\nbreak",
            "é\",tier=\"hot\\\"} 0\ninjected 1\n#",
        ] {
            let metrics = Metrics::new();
            let accounting = metrics.tenant_accounting(tenant);
            accounting.record_occupancy(1_250_000);
            accounting.record_completion(true);
            let text = metrics.to_text();
            let samples: Vec<_> = text.lines().filter(|line| !line.starts_with('#')).collect();
            let tenant_samples: Vec<_> = samples
                .iter()
                .filter(|line| line.starts_with("isoplane_tenant_"))
                .map(|line| {
                    let (series, value) = line.rsplit_once(' ').unwrap();
                    (series.to_owned(), value.parse::<f64>().unwrap())
                })
                .collect();
            let label = serde_json::to_string(tenant).unwrap();
            assert_eq!(
                tenant_samples,
                [
                    (format!("isoplane_tenant_dispatches{{tenant={label}}}"), 1.0),
                    (
                        format!("isoplane_tenant_occupancy_ms{{tenant={label}}}"),
                        1.25
                    ),
                    (format!("isoplane_tenant_traps{{tenant={label}}}"), 1.0),
                ],
                "tenant {tenant:?} changed its samples"
            );
            assert!(
                samples.iter().all(|line| line.starts_with("isoplane_cell_")
                    || line.starts_with("isoplane_tenant_")),
                "a label injected a sample: {text}"
            );
        }
    }

    #[test]
    fn simultaneous_writers_replace_the_entire_retained_window() {
        let metrics = Metrics {
            dispatch_us: Histogram::new(4),
            ..Metrics::default()
        };
        for value in 0..4 {
            metrics.dispatch_us.record(value);
        }
        let ready = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            for value in 100..104 {
                let histogram = &metrics.dispatch_us;
                let ready = &ready;
                scope.spawn(move || {
                    ready.wait();
                    histogram.record(value);
                });
            }
            ready.wait();
        });
        assert_eq!(metrics.dispatch_us.p50(), 101);
        assert_eq!(metrics.dispatch_us.p99(), 102);
        assert_eq!(metrics.dispatch_us.count(), 8);
        assert_eq!(metrics.dispatch_us.sum(), 412);
    }

    #[test]
    fn the_window_evicts_the_oldest_sample_and_totals_remain_lifetime() {
        let histogram = Histogram::new(2);
        for value in [90, 10, 20] {
            histogram.record(value);
        }
        assert_eq!(histogram.percentile(0.0), 10);
        assert_eq!(histogram.percentile(1.0), 20);
        assert_eq!(histogram.count(), 3);
        assert_eq!(histogram.sum(), 120);
        assert_eq!(histogram.state.lock().samples.len(), 2);
    }

    #[test]
    fn zero_capacity_retains_only_lifetime_totals() {
        let histogram = Histogram::new(0);
        histogram.record(u64::MAX);
        histogram.record(u64::MAX);
        assert_eq!(histogram.p99(), 0);
        assert_eq!(histogram.count(), 2);
        assert_eq!(histogram.sum(), 2 * u128::from(u64::MAX));
        assert!(histogram.state.lock().samples.is_empty());
    }

    #[test]
    fn an_exhausted_count_keeps_the_last_valid_distribution() {
        let histogram = Histogram::new(1);
        {
            let mut state = histogram.state.lock();
            state.count = u64::MAX - 1;
            state.sum = u128::from(u64::MAX - 1) * u128::from(u64::MAX);
        }
        histogram.record(u64::MAX);
        histogram.record(0);
        assert_eq!(histogram.count(), u64::MAX);
        assert_eq!(histogram.sum(), u128::from(u64::MAX) * u128::from(u64::MAX));
        assert_eq!(histogram.p99(), u64::MAX);
    }

    #[test]
    fn percentiles_are_exact_over_the_window() {
        let h = Histogram::new(1000);
        for i in 1..=100u64 {
            h.record(i);
        }
        assert_eq!(h.count(), 100, "every sample is counted");
        assert_eq!(h.p50(), 50, "nearest rank, lower inclusive");
        assert_eq!(h.p99(), 99, "p99 of 1..=100");
        assert_eq!(h.sum(), 5_050, "the sum of 1..=100");
    }

    #[test]
    fn an_empty_histogram_reads_zero_rather_than_panicking() {
        let h = Histogram::default();
        assert_eq!(h.p50(), 0, "no samples, no p50");
        assert_eq!(h.p99(), 0, "no samples, no p99");
        assert_eq!(h.sum(), 0, "no samples, no sum");
    }

    #[test]
    fn the_scrape_carries_every_metric_name() {
        let m = Metrics::new();
        m.dispatches.fetch_add(7, Ordering::Relaxed);
        m.traps.fetch_add(1, Ordering::Relaxed);
        m.activation_us.record(900);
        m.hibernation_wake_us.record(40);
        m.dirty_pages.record(512);
        m.delta_bytes.fetch_add(4096, Ordering::Relaxed);
        let accounting = m.tenant_accounting("acme");
        accounting.record_occupancy(2_000_000);
        accounting.record_completion(false);

        let text = m.to_text();
        for needle in [
            "isoplane_cell_dispatches 7",
            "isoplane_cell_activation_p50_us 900",
            "isoplane_cell_hibernation_wake_p50_us 40",
            "isoplane_cell_dirty_pages_p50 512",
            "isoplane_cell_delta_bytes 4096",
            "isoplane_cell_instances_parked",
            "isoplane_cell_instances_hibernated",
            "isoplane_tenant_occupancy_ms{tenant=\"acme\"} 2",
        ] {
            assert!(
                text.contains(needle),
                "scrape is missing `{needle}`:\n{text}"
            );
        }

        m.delta_bytes.store(u64::MAX, Ordering::Relaxed);
        let json: serde_json::Value = serde_json::from_str(&m.to_json()).expect("json");
        assert_eq!(
            json["delta_bytes"].as_u64(),
            Some(u64::MAX),
            "JSON keeps integer precision"
        );
        assert_eq!(
            json["dispatches"], 7,
            "the json scrape carries the counters"
        );
        assert_eq!(
            json["tenants"]["acme"]["dispatches"], 1,
            "and the per-tenant counters"
        );
    }
}
