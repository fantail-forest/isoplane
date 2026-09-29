//! Node measurement interfaces and latency summaries.


use serde::{Deserialize, Serialize};

pub trait NodeAdmin: Send + Sync + 'static {




    /// Current node measurements.
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
    /// Nearest-rank quantiles over a sample vector.
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
    /// Recorded CPU: the thread clock over hot intervals, summed over tenants.
    pub cpu_ms_total: f64,
    pub activation: Latency,
    pub wake: Latency,
    pub instances_hot: u64,
    pub instances_parked: u64,
    pub instances_hibernated: u64,
    /// Raw samples; the exporter reduces them.
    pub dirty_set_bytes: Vec<u64>,
    pub delta_storage_bytes: u64,
    pub traps_total: u64,
    pub breaker_trips: u64,
}
