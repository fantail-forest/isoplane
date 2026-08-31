//! Local preparation and execution cells behind the HTTP dispatcher.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use parking_lot::RwLock;
use wisp_abi::host::RouteTarget;

// The real data plane: engine + cells behind the proxy's `Dispatcher`.

use std::path::Path;

use wisp_abi::dispatch::HttpRequest;
use wisp_abi::host::{Dispatcher, HttpReply};
use wisp_host::ModuleId;
use wisp_host::{Cell, EngineActivator};

/// A node's cells, wired to the real engine, and the adapter that puts them
/// behind [`wisp_abi::host::Dispatcher`].
///
/// The adapter lives in `wispd` rather than in `wisp-host` because it is
/// assembly: it needs to know the node's *placement* (which app runs on which
/// cell), which is a property of this node's configuration and not of any
/// cell. `wisp-host` deliberately knows nothing about routes.
pub struct RealCells {
    engine: Arc<wisp_engine::WispEngine>,
    cells: Vec<Arc<Cell>>,
    /// Configured application-to-cell assignment.
    placement: RwLock<HashMap<String, usize>>,
    /// `app@version` -> the module registered for it.
    modules: RwLock<HashMap<String, ModuleId>>,
    tenants: Arc<RwLock<HashMap<String, String>>>,
}

/// What a real node needs on disk. All local (docs/plan.md: "Local storage").
pub struct RealPaths {
    /// Private local preparation output.
    pub registry: std::path::PathBuf,
    /// Compiled template artifacts, one `.meta` + `.cwasm` pair per
    /// `(module hash, engine version, µarch)`.
    ///
    /// Separate from `registry` because these are *mapped*, not read: a cell
    /// loads a template with `Module::deserialize_file`, so the bytes have to
    /// be a bare `.cwasm` starting at offset 0 of a file this node owns and
    /// never rewrites. The registry's blobs are enveloped and content-checked
    /// on every read, which is the right shape for distribution and the wrong
    /// shape for a mapping.
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

// The admin surface over real cells.

/// Reads measurements from the execution cells.
pub struct RealNodeAdmin {
    cells: Arc<RealCells>,
    clock_skew_ms: std::sync::atomic::AtomicU64,
}

impl RealNodeAdmin {
    /// Retains the execution cells whose measurements are reported.
    pub fn new(cells: Arc<RealCells>) -> Self {
        RealNodeAdmin {
            cells,
            clock_skew_ms: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Sum the counters across cells, take the max of the quantiles. Returns
    /// `None` for a node with no cells, which `snapshot` renders as zeroes
    /// rather than inventing a `Default` the cell never defined.
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
                    // Quantiles do not add. Taking the max across cells is the
                    // conservative reading — a node is as slow as its slowest
                    // cell, and a tail hidden by averaging is a tail nobody
                    // fixes.
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
            // §11.2: recorded CPU is the thread CPU clock over Hot intervals,
            // and the cell is the only thing that can measure it.
            cpu_ms_total: 0.0,
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
            // The cell reports dirty *pages*; §10 wants bytes.
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
            // The critical bit: these are measurements, not a demo.
        }
    }
}
