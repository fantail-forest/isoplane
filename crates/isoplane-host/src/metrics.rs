//! What one cell exposes to a scrape. The histograms are per lifecycle
//! transition rather than one blended dispatch timer, so a slow dispatch
//! decomposes into a named transition.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

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

    /// Returns the lifetime arithmetic mean, or zero if empty.
    pub fn mean(&self) -> f64 {
        let state = self.state.lock();
        if state.count == 0 {
            0.0
        } else {
            state.sum as f64 / state.count as f64
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
    /// Dispatches that entered grace and returned inside it; the instance
    /// stayed.
    pub budget_exits: AtomicU64,
    /// Dispatches still executing at the end of grace, terminated without
    /// unwinding and discarded. The hang breaker counts these. Disjoint from
    /// `traps`: a kill is not a trap.
    pub budget_kills: AtomicU64,
    /// Dispatches that crossed the budget at all.
    pub grace_entries: AtomicU64,
    /// Dispatches still in progress when the grace after their cancellation
    /// ran out; each evicted its instance.
    pub cancellation_grace_kills: AtomicU64,
    /// Times an instance stopped taking new dispatches because one of its
    /// dispatches entered the grace after its cancellation.
    pub instance_holds: AtomicU64,
    /// How long each such hold lasted, until no dispatch on the instance was
    /// in its grace.
    pub instance_hold_ms: Histogram,
    /// Activations made because the instances that had room were held.
    pub activations_for_holds: AtomicU64,
    /// Instances whose memory passed their memory limit, retired once their
    /// dispatches finished.
    pub memory_limit_retirements: AtomicU64,
    /// Dispatches that still had work pending when their window after the
    /// response ran out, and handed it to their instance.
    pub work_handed_to_instance: AtomicU64,
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
    /// Egress-policy refusals, including connection-limit refusals.
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
    /// Each slice's wall time.
    pub slice_us: Histogram,
    /// How long each slice waited for its instance's thread once ready.
    pub thread_wait_us: Histogram,
    pub dirty_pages: Histogram,
    /// The pre-capture gc hint.
    pub gc_hint_us: Histogram,

    /// How a dispatch got its instance, for both instance kinds. The four are
    /// exhaustive over admitted dispatches: their sum is `dispatches`.
    pub served_warm: AtomicU64,
    pub served_shared: AtomicU64,
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
            budget_exits: l(&self.budget_exits),
            budget_kills: l(&self.budget_kills),
            grace_entries: l(&self.grace_entries),
            cancellation_grace_kills: l(&self.cancellation_grace_kills),
            instance_holds: l(&self.instance_holds),
            instance_hold_p50_ms: self.instance_hold_ms.p50(),
            instance_hold_p99_ms: self.instance_hold_ms.p99(),
            activations_for_holds: l(&self.activations_for_holds),
            memory_limit_retirements: l(&self.memory_limit_retirements),
            work_handed_to_instance: l(&self.work_handed_to_instance),
            grace_deferrals: l(&self.grace_deferrals),
            overage_ns: l(&self.overage_ns),
            // With no kills the ratio is reported as the exit count, the most
            // it can claim.
            exit_kill_ratio: {
                let (c, h) = (l(&self.budget_exits), l(&self.budget_kills));
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
            served_shared: l(&self.served_shared),
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
            slice_p50_us: self.slice_us.p50(),
            slice_p99_us: self.slice_us.p99(),
            thread_wait_p50_us: self.thread_wait_us.p50(),
            thread_wait_p99_us: self.thread_wait_us.p99(),
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

    /// Encodes the current diagnostic snapshot as Prometheus gauges.
    pub fn to_text(&self) -> String {
        let snapshot = self.snapshot();
        let mut families: Vec<_> = snapshot
            .values()
            .into_iter()
            .map(|(name, value)| {
                gauge_family(
                    &format!("isoplane_cell_{name}"),
                    vec![(None, value.as_f64())],
                )
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
    pub wakes: u64,
    pub in_cell_wakes: u64,
    pub wakes_dropped: u64,
    pub traps: u64,
    pub budget_exits: u64,
    pub budget_kills: u64,
    pub grace_entries: u64,
    pub cancellation_grace_kills: u64,
    pub instance_holds: u64,
    pub instance_hold_p50_ms: u64,
    pub instance_hold_p99_ms: u64,
    pub activations_for_holds: u64,
    pub memory_limit_retirements: u64,
    pub work_handed_to_instance: u64,
    pub grace_deferrals: u64,
    pub overage_ns: u64,
    pub exit_kill_ratio: f64,
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
    pub served_shared: u64,
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
    pub slice_p50_us: u64,
    pub slice_p99_us: u64,
    pub thread_wait_p50_us: u64,
    pub thread_wait_p99_us: u64,
    pub clock_read_failures: u64,
    pub trap_rate: f64,
    pub tenants: BTreeMap<String, TenantCounters>,
}

#[derive(Clone, Copy)]
enum MetricValue {
    Count(u64),
    Fraction(f64),
}

impl MetricValue {
    fn as_f64(self) -> f64 {
        match self {
            Self::Count(value) => value as f64,
            Self::Fraction(value) => value,
        }
    }

    fn as_json(self) -> serde_json::Value {
        match self {
            Self::Count(value) => value.into(),
            Self::Fraction(value) => value.into(),
        }
    }
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
    fn values(&self) -> Vec<(&'static str, MetricValue)> {
        vec![
            ("dispatches", MetricValue::Count(self.dispatches)),
            ("wakes", MetricValue::Count(self.wakes)),
            ("in_cell_wakes", MetricValue::Count(self.in_cell_wakes)),
            ("wakes_dropped", MetricValue::Count(self.wakes_dropped)),
            ("traps", MetricValue::Count(self.traps)),
            ("budget_exits", MetricValue::Count(self.budget_exits)),
            ("budget_kills", MetricValue::Count(self.budget_kills)),
            ("grace_entries", MetricValue::Count(self.grace_entries)),
            (
                "cancellation_grace_kills",
                MetricValue::Count(self.cancellation_grace_kills),
            ),
            ("instance_holds", MetricValue::Count(self.instance_holds)),
            (
                "instance_hold_p50_ms",
                MetricValue::Count(self.instance_hold_p50_ms),
            ),
            (
                "instance_hold_p99_ms",
                MetricValue::Count(self.instance_hold_p99_ms),
            ),
            (
                "activations_for_holds",
                MetricValue::Count(self.activations_for_holds),
            ),
            (
                "memory_limit_retirements",
                MetricValue::Count(self.memory_limit_retirements),
            ),
            (
                "work_handed_to_instance",
                MetricValue::Count(self.work_handed_to_instance),
            ),
            ("grace_deferrals", MetricValue::Count(self.grace_deferrals)),
            (
                "overage_ms",
                MetricValue::Fraction(self.overage_ns as f64 / 1e6),
            ),
            (
                "exit_kill_ratio",
                MetricValue::Fraction(self.exit_kill_ratio),
            ),
            ("shed", MetricValue::Count(self.shed)),
            (
                "breaker_rejections",
                MetricValue::Count(self.breaker_rejections),
            ),
            (
                "reentrancy_refusals",
                MetricValue::Count(self.reentrancy_refusals),
            ),
            (
                "pending_cap_rejections",
                MetricValue::Count(self.pending_cap_rejections),
            ),
            ("egress_denials", MetricValue::Count(self.egress_denials)),
            ("instances_live", MetricValue::Count(self.live)),
            ("instances_parked", MetricValue::Count(self.parked)),
            ("instances_hibernated", MetricValue::Count(self.hibernated)),
            ("instances_hot", MetricValue::Count(self.hot)),
            ("captures", MetricValue::Count(self.captures)),
            ("delta_bytes", MetricValue::Count(self.delta_bytes)),
            (
                "capture_write_failures",
                MetricValue::Count(self.capture_write_failures),
            ),
            (
                "hibernation_wakes",
                MetricValue::Count(self.hibernation_wakes),
            ),
            ("delta_fallbacks", MetricValue::Count(self.delta_fallbacks)),
            ("futures_shed", MetricValue::Count(self.futures_shed)),
            (
                "futures_shed_capture",
                MetricValue::Count(self.futures_shed_capture),
            ),
            (
                "futures_buffered",
                MetricValue::Count(self.futures_buffered),
            ),
            ("served_warm", MetricValue::Count(self.served_warm)),
            ("served_shared", MetricValue::Count(self.served_shared)),
            (
                "served_hibernation_wake",
                MetricValue::Count(self.served_hibernation_wake),
            ),
            (
                "served_activated",
                MetricValue::Count(self.served_activated),
            ),
            (
                "activation_p50_us",
                MetricValue::Count(self.activation_p50_us),
            ),
            (
                "activation_p99_us",
                MetricValue::Count(self.activation_p99_us),
            ),
            ("wake_p50_us", MetricValue::Count(self.wake_p50_us)),
            ("wake_p99_us", MetricValue::Count(self.wake_p99_us)),
            (
                "hibernation_wake_p50_us",
                MetricValue::Count(self.hibernation_wake_p50_us),
            ),
            (
                "hibernation_wake_p99_us",
                MetricValue::Count(self.hibernation_wake_p99_us),
            ),
            ("capture_p50_us", MetricValue::Count(self.capture_p50_us)),
            ("dispatch_p50_us", MetricValue::Count(self.dispatch_p50_us)),
            ("dispatch_p99_us", MetricValue::Count(self.dispatch_p99_us)),
            ("gc_hint_p50_us", MetricValue::Count(self.gc_hint_p50_us)),
            ("dirty_pages_p50", MetricValue::Count(self.dirty_pages_p50)),
            ("dirty_pages_p99", MetricValue::Count(self.dirty_pages_p99)),
            ("park_p50_ms", MetricValue::Count(self.park_p50_ms)),
            ("slice_p50_us", MetricValue::Count(self.slice_p50_us)),
            ("slice_p99_us", MetricValue::Count(self.slice_p99_us)),
            (
                "thread_wait_p50_us",
                MetricValue::Count(self.thread_wait_p50_us),
            ),
            (
                "thread_wait_p99_us",
                MetricValue::Count(self.thread_wait_p99_us),
            ),
            (
                "clock_read_failures",
                MetricValue::Count(self.clock_read_failures),
            ),
            ("trap_rate", MetricValue::Fraction(self.trap_rate)),
        ]
    }

    pub fn as_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        for (name, value) in self.values() {
            m.insert(name.to_owned(), value.as_json());
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
            metrics.record_tenant(tenant, 1_250_000, true);
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
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.dispatch_p50_us, 101);
        assert_eq!(snapshot.dispatch_p99_us, 102);
        assert_eq!(metrics.dispatch_us.count(), 8);
        assert_eq!(metrics.dispatch_us.mean(), 51.5);
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
        assert_eq!(histogram.mean(), 40.0);
        assert_eq!(histogram.state.lock().samples.len(), 2);
    }

    #[test]
    fn zero_capacity_retains_only_lifetime_totals() {
        let histogram = Histogram::new(0);
        histogram.record(u64::MAX);
        histogram.record(u64::MAX);
        assert_eq!(histogram.p99(), 0);
        assert_eq!(histogram.count(), 2);
        assert_eq!(histogram.mean(), u64::MAX as f64);
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
        assert_eq!(histogram.mean(), u64::MAX as f64);
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
    fn the_exit_kill_ratio_is_a_scraped_metric() {
        let m = Metrics::new();
        for _ in 0..3_000 {
            m.budget_exits.fetch_add(1, Ordering::Relaxed);
        }
        m.budget_kills.fetch_add(1, Ordering::Relaxed);
        let s = m.snapshot();
        assert_eq!(
            s.exit_kill_ratio, 3_000.0,
            "3,000 exits against 1 kill is 3,000"
        );
        assert!(
            m.to_text().contains("isoplane_cell_exit_kill_ratio 3000"),
            "the ratio is on the text scrape"
        );
        let json: serde_json::Value = serde_json::from_str(&m.to_json()).expect("json");
        assert_eq!(
            json["budget_exits"], 3_000,
            "the exit count is on the json scrape"
        );
        assert_eq!(json["budget_kills"], 1, "so is the kill count");
    }

    #[test]
    fn the_exit_kill_ratio_can_report_an_unhealthy_fleet() {
        let m = Metrics::new();
        m.budget_exits.fetch_add(1, Ordering::Relaxed);
        for _ in 0..1_000 {
            m.budget_kills.fetch_add(1, Ordering::Relaxed);
        }
        assert!(
            m.snapshot().exit_kill_ratio < 0.01,
            "a fleet that is all kills must read as such, not as healthy"
        );
        assert_eq!(
            Metrics::new().snapshot().exit_kill_ratio,
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
            "isoplane_tenant_occupancy_ms{tenant=\"acme\"} 2",
        ] {
            assert!(
                text.contains(needle),
                "scrape is missing `{needle}`:\n{text}"
            );
        }

        m.wakes.store(u64::MAX, Ordering::Relaxed);
        let json: serde_json::Value = serde_json::from_str(&m.to_json()).expect("json");
        assert_eq!(
            json["wakes"].as_u64(),
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
        assert!(
            (json["trap_rate"].as_f64().expect("a number") - 1.0 / 7.0).abs() < 1e-6,
            "the trap rate is traps over dispatches"
        );
    }
}
