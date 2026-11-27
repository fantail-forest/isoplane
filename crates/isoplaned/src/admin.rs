//! Read-only local host measurements.

use serde::{Deserialize, Serialize};

/// Read-only measurements of the local execution pool.
pub trait NodeAdmin: Send + Sync + 'static {
    /// Returns the current counters and latency summaries.
    fn snapshot(&self) -> NodeSnapshot;
}

/// Quantiles and lifetime totals for a latency series.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Latency {
    pub p50_us: u64,
    pub p99_us: u64,
    pub count: u64,
    /// Sum of the observations in microseconds.
    pub sum_us: f64,
}

/// Measured size distribution across completed snapshot captures.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct SnapshotSizes {
    /// Median dirty bytes in a capture.
    pub p50_bytes: u64,
    /// Ninety-ninth percentile dirty bytes in a capture.
    pub p99_bytes: u64,
    /// Number of completed capture observations.
    pub count: u64,
    /// Sum of dirty bytes across those observations.
    pub sum_bytes: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeSnapshot {
    pub dispatches_total: u64,
    /// Measured occupancy: monotonic time over active worker intervals, summed over tenants.
    pub occupancy_ms_total: f64,
    pub activation: Latency,
    pub wake: Latency,
    pub instances_hot: u64,
    pub instances_parked: u64,
    pub instances_hibernated: u64,
    /// Measured capture distribution, without inventing individual samples.
    pub dirty_set: SnapshotSizes,
    /// Total bytes written by completed local captures.
    pub snapshot_written_bytes: u64,
    pub traps_total: u64,
    /// Requests refused by the hang breaker, including retained-path capacity refusals.
    pub breaker_rejections: u64,
}
