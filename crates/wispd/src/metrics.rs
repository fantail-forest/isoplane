//! Renders node and proxy measurements as Prometheus text.

use std::fmt::Write as _;

use wisp_proxy::ProxyMetrics;

use crate::admin::{Latency, NodeSnapshot};

fn latency_lines(out: &mut String, node: &str, name: &str, help: &str, l: Latency) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} summary");
    let _ = writeln!(out, "{name}{{node=\"{node}\",quantile=\"0.5\"}} {}", l.p50_us);
    let _ = writeln!(out, "{name}{{node=\"{node}\",quantile=\"0.99\"}} {}", l.p99_us);
    let _ = writeln!(out, "{name}_count{{node=\"{node}\"}} {}", l.count);
}

fn histogram_lines(
    out: &mut String,
    node: &str,
    name: &str,
    help: &str,
    samples: &[u64],
    buckets: &[u64],
) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} histogram");
    // Prometheus buckets are cumulative.
    for b in buckets {
        let cum = samples.iter().filter(|s| **s <= *b).count();
        let _ = writeln!(out, "{name}_bucket{{node=\"{node}\",le=\"{b}\"}} {cum}");
    }
    let _ = writeln!(out, "{name}_bucket{{node=\"{node}\",le=\"+Inf\"}} {}", samples.len());
    let _ = writeln!(out, "{name}_sum{{node=\"{node}\"}} {}", samples.iter().sum::<u64>());
    let _ = writeln!(out, "{name}_count{{node=\"{node}\"}} {}", samples.len());
}

/// Renders the whole node's meters.
pub fn render(node: &str, proxy: &ProxyMetrics, snap: NodeSnapshot) -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let mut o = String::with_capacity(8192);
    let n = |name: &str| format!("{name}{{node=\"{node}\"}}");


    // --- dispatches & CPU ------------------------------------------------
    let _ = writeln!(&mut o, "# HELP wisp_dispatches_total Dispatches handled, by code tier");
    let _ = writeln!(&mut o, "# TYPE wisp_dispatches_total counter");
    let _ = writeln!(
        &mut o,
        "wisp_dispatches_total{{node=\"{node}\",tier=\"fast\"}} {}",
        proxy.dispatches_tier_fast.load(Relaxed)
    );
    let _ = writeln!(
        &mut o,
        "wisp_dispatches_total{{node=\"{node}\",tier=\"optimized\"}} {}",
        proxy.dispatches_tier_optimized.load(Relaxed)
    );
    let _ = writeln!(&mut o, "# HELP wisp_cell_dispatches_total Dispatches observed inside the cell");
    let _ = writeln!(&mut o, "# TYPE wisp_cell_dispatches_total counter");
    let _ = writeln!(&mut o, "{} {}", n("wisp_cell_dispatches_total"), snap.dispatches_total);

    let _ = writeln!(
        &mut o,
        "# HELP wisp_cpu_ms_total CPU milliseconds reported by the node"
    );
    let _ = writeln!(&mut o, "# TYPE wisp_cpu_ms_total counter");
    let _ = writeln!(&mut o, "{} {:.3}", n("wisp_cpu_ms_total"), snap.cpu_ms_total);

    // --- activation & wake latency --------------------------------------
    latency_lines(
        &mut o,
        node,
        "wisp_activation_us",
        "Cold activation from a local template",
        snap.activation,
    );
    latency_lines(
        &mut o,
        node,
        "wisp_wake_us",
        "Reported local wake latency",
        snap.wake,
    );

    // --- lifecycle tier counts ------------------------------------------
    let _ = writeln!(
        &mut o,
        "# HELP wisp_instances Instances by lifecycle state"
    );
    let _ = writeln!(&mut o, "# TYPE wisp_instances gauge");
    for (tier, v) in [
        ("hot", snap.instances_hot),
        ("parked", snap.instances_parked),
        ("hibernated", snap.instances_hibernated),
    ] {
        let _ = writeln!(&mut o, "wisp_instances{{node=\"{node}\",tier=\"{tier}\"}} {v}");
    }

    // --- dirty set & delta storage --------------------------------------
    histogram_lines(
        &mut o,
        node,
        "wisp_dirty_set_bytes",
        "Reported per-instance dirty-set bytes",
        &snap.dirty_set_bytes,
        &[64 * 1024, 256 * 1024, 1 << 20, 4 << 20, 16 << 20, 64 << 20],
    );
    let _ = writeln!(&mut o, "# HELP wisp_delta_storage_bytes Local snapshot bytes held");
    let _ = writeln!(&mut o, "# TYPE wisp_delta_storage_bytes gauge");
    let _ = writeln!(&mut o, "{} {}", n("wisp_delta_storage_bytes"), snap.delta_storage_bytes);

    // --- traps ------------------------------------------------

    let _ = writeln!(&mut o, "# HELP wisp_traps_total Guest faults. Traps are never retried on the same memory.");
    let _ = writeln!(&mut o, "# TYPE wisp_traps_total counter");
    let _ = writeln!(&mut o, "{} {}", n("wisp_traps_total"), snap.traps_total);
    let _ = writeln!(&mut o, "# HELP wisp_trap_rate Traps per dispatch");
    let _ = writeln!(&mut o, "# TYPE wisp_trap_rate gauge");
    let denom = snap.dispatches_total.max(1) as f64;
    let _ = writeln!(&mut o, "{} {:.6}", n("wisp_trap_rate"), snap.traps_total as f64 / denom);
    let _ = writeln!(&mut o, "# HELP wisp_hang_breakers_tripped Routes currently failing fast");
    let _ = writeln!(&mut o, "# TYPE wisp_hang_breakers_tripped gauge");
    let _ = writeln!(&mut o, "{} {}", n("wisp_hang_breakers_tripped"), snap.breaker_trips);

    // --- proxy-owned meters ---------------------------------------------------
    for (name, help, ty, v) in [
        ("wisp_proxy_requests_total", "Requests accepted by the front proxy", "counter", proxy.requests_total.load(Relaxed)),
        ("wisp_proxy_requests_unrouted_total", "Requests with no matching route", "counter", proxy.requests_404.load(Relaxed)),
        ("wisp_proxy_requests_shed_total", "Requests refused at the edge before cells saturated", "counter", proxy.requests_shed.load(Relaxed)),
        ("wisp_proxy_requests_failed_total", "Dispatches that returned an error", "counter", proxy.requests_failed.load(Relaxed)),
        ("wisp_proxy_route_generation", "Route-table generation", "gauge", proxy.route_generation.load(Relaxed)),
    ] {
        let _ = writeln!(&mut o, "# HELP {name} {help}");
        let _ = writeln!(&mut o, "# TYPE {name} {ty}");
        let _ = writeln!(&mut o, "{} {}", n(name), v);
    }

    for (name, help, h) in [
        ("wisp_proxy_request_latency_us", "End-to-end latency observed at the proxy", &proxy.request_latency),
        ("wisp_proxy_admission_wait_us", "Time spent queued at admission before a slot freed", &proxy.admission_wait),
    ] {
        let _ = writeln!(&mut o, "# HELP {name} {help}");
        let _ = writeln!(&mut o, "# TYPE {name} summary");
        let _ = writeln!(&mut o, "{name}{{node=\"{node}\",quantile=\"0.5\"}} {:.0}", h.p50_us());
        let _ = writeln!(&mut o, "{name}{{node=\"{node}\",quantile=\"0.99\"}} {:.0}", h.p99_us());
        let _ = writeln!(&mut o, "{name}_count{{node=\"{node}\"}} {}", h.count());
    }

    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    fn snap() -> NodeSnapshot {
        NodeSnapshot {
            dispatches_total: 12,
            cpu_ms_total: 4.25,
            activation: Latency::from_samples(vec![300, 700, 900, 4000]),
            wake: Latency::from_samples(vec![8000, 9000, 40000]),
            instances_hot: 1,
            instances_parked: 3,
            instances_hibernated: 7,
            dirty_set_bytes: vec![100 * 1024, 900 * 1024, 3 << 20],
            delta_storage_bytes: 5 << 20,
            traps_total: 1,
            breaker_trips: 0,
        }
    }

    #[test]
    fn every_section_of_the_ten_dot_metrics_list_is_present() {
        let p = ProxyMetrics::new();
        p.requests_total.store(9, Relaxed);
        p.dispatches_tier_fast.store(4, Relaxed);
        let out = render("n1", &p, snap());
        for series in [
            "wisp_dispatches_total",
            "wisp_cpu_ms_total",
            "wisp_activation_us",
            "wisp_wake_us",
            "wisp_instances",
            "wisp_dirty_set_bytes",
            "wisp_delta_storage_bytes",
            "wisp_trap_rate",
        ] {
            assert!(out.contains(series), "missing {series} from /metrics");
        }
        // §9.3: tier=fast must be visible.
        assert!(out.contains("tier=\"fast\"} 4"));
    }

    #[test]
    fn quantiles_come_out_of_the_samples() {
        let out = render("n1", &ProxyMetrics::new(), snap());
        assert!(out.contains("wisp_activation_us{node=\"n1\",quantile=\"0.5\"} 700"), "{out}");
        assert!(out.contains("wisp_wake_us{node=\"n1\",quantile=\"0.5\"} 9000"), "{out}");
    }


    #[test]
    fn an_empty_node_renders_without_dividing_by_zero() {
        let out = render("n1", &ProxyMetrics::new(), NodeSnapshot::default());
        assert!(out.contains("wisp_trap_rate{node=\"n1\"} 0.000000"));
        assert!(!out.contains("NaN"));
    }
}
