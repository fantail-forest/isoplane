//! Local preparation and execution cells behind the HTTP dispatcher.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use wisp_abi::host::RouteTarget;

use std::path::Path;

use wisp_abi::dispatch::HttpRequest;
use wisp_abi::host::{Dispatcher, HttpReply};
use wisp_host::ModuleId;
use wisp_host::{Cell, EngineActivator};

/// The node's cells on one engine, and the placement that puts an app on
/// one of them.
pub struct RealCells {
    engine: Arc<wisp_engine::WispEngine>,
    cells: Vec<Arc<Cell>>,
    /// Configured application-to-cell assignment.
    placement: RwLock<HashMap<String, usize>>,
    /// `app@version` -> the module registered for it.
    modules: RwLock<HashMap<String, ModuleId>>,
    tenants: Arc<RwLock<HashMap<String, String>>>,
}

/// What a real node keeps on disk.
pub struct RealPaths {
    /// Private local preparation output.
    pub registry: std::path::PathBuf,
    /// Compiled templates, one `.meta` and `.cwasm` pair per
    /// `(module hash, engine version, micro-architecture)`. Apart from the
    /// registry because these files are mapped and never rewritten.
    pub artifacts: std::path::PathBuf,
}

impl RealPaths {
    pub fn under(root: &Path) -> Self {
        RealPaths {
            registry: root.join("registry"),
            artifacts: root.join("artifacts"),
        }
    }
}

impl RealCells {
    pub fn cells(&self) -> &[Arc<Cell>] {
        &self.cells
    }

    fn key(app: &str, version: &str) -> String {
        format!("{app}@{version}")
    }
}

/// Dispatches to the application's configured local cell.
#[async_trait::async_trait]
impl Dispatcher for RealCells {
    async fn dispatch_http(
        &self,
        target: &wisp_abi::host::RouteTarget,
        req: HttpRequest,
    ) -> anyhow::Result<HttpReply> {
        self.cell_for(&target.app)?.dispatch_http(target, req).await
    }
}

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
    fn merged(&self) -> Option<wisp_host::MetricsSnapshot> {
        let mut acc: Option<wisp_host::MetricsSnapshot> = None;
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
                        e.cpu_ns += t.cpu_ns;
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
        crate::admin::NodeSnapshot {
            dispatches_total: m.dispatches,
            cpu_ms_total: m.tenants.values().map(|t| t.cpu_ns).sum::<u64>() as f64 / 1e6,
            activation: crate::admin::Latency {
                p50_us: m.activation_p50_us,
                p99_us: m.activation_p99_us,
                count: m.dispatches,
            },
            wake: crate::admin::Latency {
                p50_us: m.hibernation_wake_p50_us,
                p99_us: m.hibernation_wake_p99_us,
                count: m.hibernation_wakes,
            },
            instances_hot: m.hot,
            instances_parked: m.parked,
            instances_hibernated: m.hibernated,
            dirty_set_bytes: vec![
                m.dirty_pages_p50 * wisp_abi::PAGE_SIZE as u64,
                m.dirty_pages_p99 * wisp_abi::PAGE_SIZE as u64,
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
