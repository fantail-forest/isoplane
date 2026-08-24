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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub dispatches_total: u64,
    pub cpu_ms_total: f64,
    /// Microsecond samples. §6.5 targets: activation p50 ≤ 1 ms, wake p50 ≤
    /// 10 ms local.
    pub activation_us: Vec<u64>,
    pub wake_us: Vec<u64>,
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
