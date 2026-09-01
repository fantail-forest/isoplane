//! Node measurement interfaces and latency summaries.

use serde::{Deserialize, Serialize};

pub trait NodeAdmin: Send + Sync + 'static {
    /// The metric list, as far as the node can honestly answer it.
    fn snapshot(&self) -> NodeSnapshot;
}

/// A latency series reduced to the quantiles the exporter publishes.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Latency {
    pub p50_us: u64,
    pub p99_us: u64,
    pub count: u64,
}

impl Latency {
    /// Nearest-rank, over a sample vector. Used by the fake; a real cell
    /// supplies its own reduction.
    pub fn from_samples(mut s: Vec<u64>) -> Self {
        if s.is_empty() {
            return Latency::default();
        }
        s.sort_unstable();
        let at = |q: f64| {
            let rank = (q * s.len() as f64).ceil().max(1.0) as usize;
            s[(rank - 1).min(s.len() - 1)]
        };
        Latency {
            p50_us: at(0.50),
            p99_us: at(0.99),
            count: s.len() as u64,
        }
    }
}

/// Dispatches, CPU-ms, p50/p99 activation and wake, live/parked/hibernated
/// instance counts, dirty-set size distribution, delta storage bytes, GC time
/// share, trap rate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub dispatches_total: u64,
    pub cpu_ms_total: f64,
    /// Pre-reduced rather than sampled because a real cell already keeps these
    /// and re-deriving them from a sample vector would be strictly less
    /// accurate than what it can hand over.
    pub activation: Latency,
    pub wake: Latency,
    pub instances_hot: u64,
    pub instances_parked: u64,
    pub instances_hibernated: u64,
    /// Deltas above a configured threshold trigger advisory telemetry, so this is
    /// kept as raw samples rather than pre-reduced quantiles: the reduction
    /// belongs in the exporter, and a test that wants the distribution should
    /// be able to have it.
    pub dirty_set_bytes: Vec<u64>,
    pub delta_storage_bytes: u64,
    pub traps_total: u64,
    pub breaker_trips: u64,
}
