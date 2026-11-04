//! `NodeAdmin` over real cells.

use std::sync::Arc;

use super::RealCells;

/// Read-only measurements from the execution cells.
pub struct RealNodeAdmin {
    cells: Arc<RealCells>,
}

impl RealNodeAdmin {
    /// Reads measurements from the supplied execution pool.
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
            return crate::admin::NodeSnapshot::default();
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
            dirty_set: crate::admin::SnapshotSizes {
                p50_bytes: m.dirty_pages_p50 * isoplane_abi::PAGE_SIZE as u64,
                p99_bytes: m.dirty_pages_p99 * isoplane_abi::PAGE_SIZE as u64,
                count: cells
                    .iter()
                    .map(|cell| cell.metrics_raw().dirty_pages.count())
                    .sum(),
                sum_bytes: cells
                    .iter()
                    .map(|cell| cell.metrics_raw().dirty_pages.sum())
                    .sum::<u128>() as f64
                    * isoplane_abi::PAGE_SIZE as f64,
            },
            snapshot_written_bytes: m.delta_bytes,
            traps_total: m.traps,
            breaker_rejections: m.breaker_rejections,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::NodeAdmin;
    use isoplane_harness::fakes::{FakeActivator, FakeBehaviour, Step};
    use isoplane_host::{Activator, ModuleId};
    use std::time::Duration;

    fn node() -> (tempfile::TempDir, Arc<RealCells>) {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = builder.tempdir().expect("private node directory");
        let mut config =
            crate::config::NodeConfig::from_toml(include_str!("../../tests/fixtures/node.toml"))
                .expect("small test engine");
        config.data_dir = directory.path().to_owned();
        config.host = isoplane_host::CellConfig::for_test("metrics");
        config.host.breaker.kill_threshold = 1;
        let cells = RealCells::build(&config, &super::super::RealPaths::under(directory.path()))
            .expect("node accounting");
        (directory, cells)
    }

    fn register(cell: &Arc<isoplane_host::Cell>, behaviour: FakeBehaviour) -> ModuleId {
        let activator = Arc::new(FakeActivator::named(ModuleId::new("app", "v1"), behaviour));
        let module = activator.module().clone();
        cell.register_module(
            "owner",
            activator,
            toml::from_str("name='app'").expect("manifest"),
        )
        .expect("registered module");
        module
    }

    fn dispatch(
        cell: &Arc<isoplane_host::Cell>,
        module: &ModuleId,
    ) -> isoplane_host::DispatchResult {
        let request = isoplane_abi::HttpRequest {
            method: "GET".into(),
            url: "http://app.example/".into(),
            path: "/".into(),
            query: vec![],
            headers: vec![],
            body: vec![],
        };
        cell.dispatch_blocking(
            module,
            isoplane_host::cell::Target::Handler { route: "/".into() },
            cell.http_record(module, request),
            Duration::from_secs(10),
        )
        .expect("dispatch result")
    }

    fn scrape(cells: Arc<RealCells>) -> String {
        let admin = RealNodeAdmin::new(cells);
        crate::metrics::render(
            "node",
            &isoplane_proxy::ProxyMetrics::new(),
            admin.snapshot(),
        )
    }

    #[test]
    fn the_public_scrape_includes_reactor_work_after_dispatch_completion_and_retirement() {
        let (_directory, cells) = node();
        let cell = &cells.cells()[0];
        let mut behaviour = FakeBehaviour::done();
        behaviour.slice_occupancy = Some(Duration::from_millis(2));
        let module = register(cell, behaviour);
        assert_eq!(dispatch(cell, &module).status, 200);
        let meters = cell.meters(&module).expect("application meter");
        meters.record_interval(Some(3_000_000));
        let text = scrape(cells.clone());
        assert!(
            text.lines()
                .any(|line| line == "isoplane_occupancy_ms_total{node=\"node\"} 5"),
            "the scrape must include both the guest slice and reactor interval: {text}"
        );
        assert!(cell.retire_module(&module));
        meters.record_interval(Some(250_000));
        register(cell, FakeBehaviour::done());
        let text = scrape(cells.clone());
        assert!(
            text.lines()
                .any(|line| line == "isoplane_occupancy_ms_total{node=\"node\"} 5.25"),
            "retirement and replacement must retain lifetime usage and late reactor polls: {text}"
        );
        cell.shutdown();
    }

    #[test]
    fn the_public_scrape_counts_breaker_rejections_and_keeps_them_after_recovery() {
        let (_directory, cells) = node();
        let cell = &cells.cells()[0];
        let module = register(cell, FakeBehaviour::program([Step::HardKill]));
        assert_eq!(
            dispatch(cell, &module).completion,
            isoplane_engine::Completion::BudgetKill
        );
        for _ in 0..2 {
            assert_eq!(
                dispatch(cell, &module).trap.as_deref(),
                Some("breaker_open")
            );
        }
        let before = scrape(cells.clone());
        cell.clear_breaker(&module);
        register(cell, FakeBehaviour::done());
        assert_eq!(dispatch(cell, &module).status, 200);
        let after = scrape(cells.clone());
        for text in [before, after] {
            assert!(
                text.lines()
                    .any(|line| line == "# TYPE isoplane_hang_breaker_rejections_total counter"),
                "{text}"
            );
            assert!(
                text.lines()
                    .any(|line| line == "isoplane_hang_breaker_rejections_total{node=\"node\"} 2"),
                "recovery preserves the cumulative refusal count: {text}"
            );
        }
        cell.shutdown();
    }
}
