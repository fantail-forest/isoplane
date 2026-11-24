use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

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

    pub breaker_rejections: AtomicU64,
    /// Lifecycle transitions slots refused. A refusal is a host path acting
    /// on an instance another path already moved, so any count is a host
    /// defect.
    pub refused_transitions: AtomicU64,

    pub parked: AtomicU64,
    pub hibernated: AtomicU64,
    pub hot: AtomicU64,

    /// Cumulative bytes written to local snapshot reservations.
    pub delta_bytes: AtomicU64,
    pub activation_us: Histogram,
    pub hibernation_wake_us: Histogram,
    pub dirty_pages: Histogram,

    by_tenant: Mutex<BTreeMap<String, Arc<TenantMetrics>>>,

    /// Wakes the wake router enqueued.
    #[cfg(any(test, feature = "test-util"))]
    pub wakes: AtomicU64,
    /// Completions for an instance nobody could hand them to. The result is
    /// already in the future table, so for a live instance this is a dispatch
    /// that is never answered; for a discarded one it is the correct outcome.
    #[cfg(any(test, feature = "test-util"))]
    pub wakes_dropped: AtomicU64,
    /// Dispatches that entered grace and returned inside it; the instance
    /// stayed.
    #[cfg(any(test, feature = "test-util"))]
    pub budget_exits: AtomicU64,
    /// Dispatches still executing at the end of grace, terminated without
    /// unwinding and discarded. The hang breaker counts these. Disjoint from
    /// `traps`: a kill is not a trap.
    #[cfg(any(test, feature = "test-util"))]
    pub budget_kills: AtomicU64,
    /// Occupancy consumed past the budget by exits and kills; already inside
    /// the tenant's occupancy.
    #[cfg(any(test, feature = "test-util"))]
    pub overage_ns: AtomicU64,
    /// Dispatches still in progress when the grace after their cancellation
    /// ran out; each evicted its instance.
    #[cfg(any(test, feature = "test-util"))]
    pub cancellation_grace_kills: AtomicU64,
    /// Times an instance stopped taking new dispatches because one of its
    /// dispatches entered the grace after its cancellation.
    #[cfg(any(test, feature = "test-util"))]
    pub instance_holds: AtomicU64,
    /// Activations made because the instances that had room were held.
    #[cfg(any(test, feature = "test-util"))]
    pub activations_for_holds: AtomicU64,
    /// Instances whose memory passed their memory limit, retired once their
    /// dispatches finished.
    #[cfg(any(test, feature = "test-util"))]
    pub memory_limit_retirements: AtomicU64,
    /// Dispatches that activated an instance.
    #[cfg(any(test, feature = "test-util"))]
    pub served_activated: AtomicU64,
    /// Dispatches that joined an instance already running another dispatch.
    #[cfg(any(test, feature = "test-util"))]
    pub served_shared: AtomicU64,
    #[cfg(any(test, feature = "test-util"))]
    pub shed: AtomicU64,
    #[cfg(any(test, feature = "test-util"))]
    pub pending_cap_rejections: AtomicU64,
    /// Egress-policy refusals, including connection-limit refusals.
    #[cfg(any(test, feature = "test-util"))]
    pub egress_denials: AtomicU64,
    /// Admission to final response.
    #[cfg(any(test, feature = "test-util"))]
    pub dispatch_us: Histogram,
    /// Ended dispatches by the outcome and status their completion events carry.
    #[cfg(any(test, feature = "test-util"))]
    pub dispatch_outcomes: Mutex<BTreeMap<(&'static str, u16), u64>>,
}

#[derive(Debug, Default)]
pub(crate) struct TenantMetrics {
    occupancy_ns: AtomicU64,
}

impl TenantMetrics {
    pub(crate) fn record_occupancy(&self, ns: u64) {
        self.occupancy_ns.fetch_add(ns, Ordering::Relaxed);
    }

    fn occupancy_ns(&self) -> u64 {
        self.occupancy_ns.load(Ordering::Relaxed)
    }
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
            breaker_rejections: l(&self.breaker_rejections),
            refused_transitions: l(&self.refused_transitions),
            parked: l(&self.parked),
            hibernated: l(&self.hibernated),
            hot: l(&self.hot),
            delta_bytes: l(&self.delta_bytes),
            activation_p50_us: self.activation_us.p50(),
            activation_p99_us: self.activation_us.p99(),
            hibernation_wake_p50_us: self.hibernation_wake_us.p50(),
            hibernation_wake_p99_us: self.hibernation_wake_us.p99(),
            dirty_pages_p50: self.dirty_pages.p50(),
            dirty_pages_p99: self.dirty_pages.p99(),
            tenant_occupancy_ns: self
                .by_tenant
                .lock()
                .iter()
                .map(|(tenant, metrics)| (tenant.clone(), metrics.occupancy_ns()))
                .collect(),
            #[cfg(any(test, feature = "test-util"))]
            wakes: l(&self.wakes),
            #[cfg(any(test, feature = "test-util"))]
            wakes_dropped: l(&self.wakes_dropped),
            #[cfg(any(test, feature = "test-util"))]
            budget_exits: l(&self.budget_exits),
            #[cfg(any(test, feature = "test-util"))]
            budget_kills: l(&self.budget_kills),
            #[cfg(any(test, feature = "test-util"))]
            overage_ns: l(&self.overage_ns),
            #[cfg(any(test, feature = "test-util"))]
            cancellation_grace_kills: l(&self.cancellation_grace_kills),
            #[cfg(any(test, feature = "test-util"))]
            instance_holds: l(&self.instance_holds),
            #[cfg(any(test, feature = "test-util"))]
            activations_for_holds: l(&self.activations_for_holds),
            #[cfg(any(test, feature = "test-util"))]
            memory_limit_retirements: l(&self.memory_limit_retirements),
            #[cfg(any(test, feature = "test-util"))]
            served_activated: l(&self.served_activated),
            #[cfg(any(test, feature = "test-util"))]
            served_shared: l(&self.served_shared),
            #[cfg(any(test, feature = "test-util"))]
            shed: l(&self.shed),
            #[cfg(any(test, feature = "test-util"))]
            pending_cap_rejections: l(&self.pending_cap_rejections),
            #[cfg(any(test, feature = "test-util"))]
            egress_denials: l(&self.egress_denials),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub dispatches: u64,
    pub traps: u64,
    pub breaker_rejections: u64,
    pub refused_transitions: u64,
    pub parked: u64,
    pub hibernated: u64,
    pub hot: u64,
    pub delta_bytes: u64,
    pub activation_p50_us: u64,
    pub activation_p99_us: u64,
    pub hibernation_wake_p50_us: u64,
    pub hibernation_wake_p99_us: u64,
    pub dirty_pages_p50: u64,
    pub dirty_pages_p99: u64,
    /// Each tenant's measured slice and reactor occupancy, including work
    /// after a dispatch ends.
    pub tenant_occupancy_ns: BTreeMap<String, u64>,
    #[cfg(any(test, feature = "test-util"))]
    pub wakes: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub wakes_dropped: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub budget_exits: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub budget_kills: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub overage_ns: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub cancellation_grace_kills: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub instance_holds: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub activations_for_holds: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub memory_limit_retirements: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub served_activated: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub served_shared: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub shed: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub pending_cap_rejections: u64,
    #[cfg(any(test, feature = "test-util"))]
    pub egress_denials: u64,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simultaneous_writers_replace_the_entire_retained_window() {
        let histogram = Histogram::new(4);
        for value in 0..4 {
            histogram.record(value);
        }
        let ready = std::sync::Barrier::new(5);
        std::thread::scope(|scope| {
            for value in 100..104 {
                let histogram = &histogram;
                let ready = &ready;
                scope.spawn(move || {
                    ready.wait();
                    histogram.record(value);
                });
            }
            ready.wait();
        });
        assert_eq!(histogram.p50(), 101);
        assert_eq!(histogram.p99(), 102);
        assert_eq!(histogram.count(), 8);
        assert_eq!(histogram.sum(), 412);
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
}
