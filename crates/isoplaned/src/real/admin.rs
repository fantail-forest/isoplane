//! `NodeAdmin` over real cells.

use std::sync::Arc;

use super::RealCells;

/// Reads measurements from the execution cells.
pub struct RealNodeAdmin {
    cells: Arc<RealCells>,
}

impl RealNodeAdmin {
    /// Retains the execution cells whose measurements are reported.
    pub fn new(cells: Arc<RealCells>) -> Self {
        RealNodeAdmin { cells }
    }

    /// Counters summed across cells, quantiles at their maximum, tenants
    /// merged. `None` for a node with no cells.
    fn merged(&self) -> Option<isoplane_host::MetricsSnapshot> {
        let mut acc: Option<isoplane_host::MetricsSnapshot> = None;
        for c in self.cells.cells() {
            let s = c.metrics();
            acc = Some(match acc {
                None => s,
                Some(mut a) => {
                    a.dispatches += s.dispatches;
                    a.wakes += s.wakes;
                    a.traps += s.traps;
                    a.shed += s.shed;
                    a.breaker_rejections += s.breaker_rejections;
                    a.live += s.live;
                    a.parked += s.parked;
                    a.hibernated += s.hibernated;
                    a.hot += s.hot;
                    a.delta_bytes += s.delta_bytes;
                    a.hibernation_wakes += s.hibernation_wakes;
                    for (tenant, t) in s.tenants {
                        let e = a.tenants.entry(tenant).or_default();
                        e.dispatches += t.dispatches;
                        e.occupancy_ns += t.occupancy_ns;
                        e.traps += t.traps;
                    }
                    // Quantiles do not add; a node is as slow as its slowest
                    // cell.
                    a.activation_p50_us = a.activation_p50_us.max(s.activation_p50_us);
                    a.activation_p99_us = a.activation_p99_us.max(s.activation_p99_us);
                    a.hibernation_wake_p50_us =
                        a.hibernation_wake_p50_us.max(s.hibernation_wake_p50_us);
                    a.hibernation_wake_p99_us =
                        a.hibernation_wake_p99_us.max(s.hibernation_wake_p99_us);
                    a.dirty_pages_p50 = a.dirty_pages_p50.max(s.dirty_pages_p50);
                    a.dirty_pages_p99 = a.dirty_pages_p99.max(s.dirty_pages_p99);
                    a.trap_rate = a.trap_rate.max(s.trap_rate);
                    a
                }
            });
        }
        acc
    }
}

impl crate::admin::NodeAdmin for RealNodeAdmin {





    fn snapshot(&self) -> crate::admin::NodeSnapshot {
        let Some(m) = self.merged() else {
            return crate::admin::NodeSnapshot {
                ..Default::default()
            };
        };
        let cells = self.cells.cells();
        let activation_count = cells
            .iter()
            .map(|cell| cell.metrics_raw().activation_us.count())
            .sum();
        let activation_sum_us = cells
            .iter()
            .map(|cell| cell.metrics_raw().activation_us.sum())
            .sum::<u128>();
        let wake_count = cells
            .iter()
            .map(|cell| cell.metrics_raw().hibernation_wake_us.count())
            .sum();
        let wake_sum_us = cells
            .iter()
            .map(|cell| cell.metrics_raw().hibernation_wake_us.sum())
            .sum::<u128>();
        crate::admin::NodeSnapshot {
            dispatches_total: m.dispatches,
            occupancy_ms_total: m.tenants.values().map(|t| t.occupancy_ns).sum::<u64>() as f64
                / 1e6,
            activation: crate::admin::Latency {
                p50_us: m.activation_p50_us,
                p99_us: m.activation_p99_us,
                count: activation_count,
                sum_us: activation_sum_us as f64,
            },
            wake: crate::admin::Latency {
                p50_us: m.hibernation_wake_p50_us,
                p99_us: m.hibernation_wake_p99_us,
                count: wake_count,
                sum_us: wake_sum_us as f64,
            },
            instances_hot: m.hot,
            instances_parked: m.parked,
            instances_hibernated: m.hibernated,
            dirty_set_bytes: vec![
                m.dirty_pages_p50 * isoplane_abi::PAGE_SIZE as u64,
                m.dirty_pages_p99 * isoplane_abi::PAGE_SIZE as u64,
            ]
            .into_iter()
            .filter(|b| *b > 0)
            .collect(),
            delta_storage_bytes: m.delta_bytes,
            traps_total: m.traps,
            breaker_trips: m.breaker_rejections,
        }
    }
}
