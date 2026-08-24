//! Node measurement interfaces and latency summaries.

use serde::{Deserialize, Serialize};

pub trait NodeAdmin: Send + Sync + 'static {
    /// §10's metric list, as far as the node can honestly answer it.
    fn snapshot(&self) -> NodeSnapshot;
}

/// §10: "dispatches, CPU-ms, p50/p99 activation & wake, live/parked/hibernated
/// instance counts, dirty-set size distribution, delta storage bytes, GC time
/// share, trap rate".
///
/// Samples rather than pre-reduced quantiles because the reduction belongs in
/// the exporter, and because a test that wants the raw distribution should be
/// able to have it.
/// A latency series reduced to what §10 asks for.
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub dispatches_total: u64,
    pub cpu_ms_total: f64,
    /// §6.5 targets: activation p50 ≤ 1 ms; hibernation wake p50 ≤ 10 ms on a
    /// local spool, ≤ 75 ms regional. Pre-reduced rather than sampled because
    /// a real cell already keeps these and re-deriving them from a sample
    /// vector would be strictly less accurate than what it can hand over.
    pub activation: Latency,
    pub wake: Latency,
    pub instances_hot: u64,
    pub instances_parked: u64,
    pub instances_hibernated: u64,
    /// §6.6: "typical delta: 100 KB–4 MB", and deltas above a configured threshold
    /// trigger advisory telemetry. Exported as a distribution so that advice
    /// is derivable rather than guessed.
    pub dirty_set_bytes: Vec<u64>,
    pub delta_storage_bytes: u64,
    pub traps_total: u64,
    pub breaker_trips: u64,
}
