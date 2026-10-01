//! Renders node and proxy measurements as Prometheus text.

use isoplane_proxy::ProxyMetrics;

use crate::admin::{Latency, NodeSnapshot};

fn line(out: &mut String, args: std::fmt::Arguments<'_>) {
    out.push_str(&args.to_string());
    out.push('\n');
}

fn latency_lines(out: &mut String, node: &str, name: &str, help: &str, l: Latency) {
    line(out, format_args!("# HELP {name} {help}"));
    line(out, format_args!("# TYPE {name} summary"));
    line(
        out,
        format_args!("{name}{{node=\"{node}\",quantile=\"0.5\"}} {}", l.p50_us),
    );
    line(
        out,
        format_args!("{name}{{node=\"{node}\",quantile=\"0.99\"}} {}", l.p99_us),
    );
    line(
        out,
        format_args!("{name}_count{{node=\"{node}\"}} {}", l.count),
    );
}

fn histogram_lines(
    out: &mut String,
    node: &str,
    name: &str,
    help: &str,
    samples: &[u64],
    buckets: &[u64],
) {
    line(out, format_args!("# HELP {name} {help}"));
    line(out, format_args!("# TYPE {name} histogram"));
    for b in buckets {
        let cum = samples.iter().filter(|s| **s <= *b).count();
        line(
            out,
            format_args!("{name}_bucket{{node=\"{node}\",le=\"{b}\"}} {cum}"),
        );
    }
    line(
        out,
        format_args!(
            "{name}_bucket{{node=\"{node}\",le=\"+Inf\"}} {}",
            samples.len()
        ),
    );
    line(
        out,
        format_args!(
            "{name}_sum{{node=\"{node}\"}} {}",
            samples.iter().sum::<u64>()
        ),
    );
    line(
        out,
        format_args!("{name}_count{{node=\"{node}\"}} {}", samples.len()),
    );
}

pub fn render(node: &str, proxy: &ProxyMetrics, snap: NodeSnapshot) -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let mut o = String::with_capacity(8192);
    let n = |name: &str| format!("{name}{{node=\"{node}\"}}");


    line(
        &mut o,
        format_args!("# HELP isoplane_dispatches_total Dispatches handled, by code tier"),
    );
    line(
        &mut o,
        format_args!("# TYPE isoplane_dispatches_total counter"),
    );
    line(
        &mut o,
        format_args!(
            "isoplane_dispatches_total{{node=\"{node}\",tier=\"fast\"}} {}",
            proxy.dispatches_tier_fast.load(Relaxed)
        ),
    );
    line(
        &mut o,
        format_args!(
            "isoplane_dispatches_total{{node=\"{node}\",tier=\"optimized\"}} {}",
            proxy.dispatches_tier_optimized.load(Relaxed)
        ),
    );
    line(
        &mut o,
        format_args!("# HELP isoplane_cell_dispatches_total Dispatches observed inside the cell"),
    );
    line(
        &mut o,
        format_args!("# TYPE isoplane_cell_dispatches_total counter"),
    );
    line(
        &mut o,
        format_args!(
            "{} {}",
            n("isoplane_cell_dispatches_total"),
            snap.dispatches_total
        ),
    );

    line(&mut o, format_args!("# HELP isoplane_occupancy_ms_total Measured occupied-worker milliseconds over execution and reactor intervals"
    ));
    line(
        &mut o,
        format_args!("# TYPE isoplane_occupancy_ms_total counter"),
    );
    line(
        &mut o,
        format_args!(
            "{} {:.3}",
            n("isoplane_occupancy_ms_total"),
            snap.occupancy_ms_total
        ),
    );

    latency_lines(
        &mut o,
        node,
        "isoplane_activation_us",
        "Cold activation from a local template",
        snap.activation,
    );
    latency_lines(
        &mut o,
        node,
        "isoplane_wake_us",
        "Reported local wake latency",
        snap.wake,
    );

    line(
        &mut o,
        format_args!(
            "# HELP isoplane_instances Instances by lifecycle state"
        ),
    );
    line(&mut o, format_args!("# TYPE isoplane_instances gauge"));
    for (tier, v) in [
        ("hot", snap.instances_hot),
        ("parked", snap.instances_parked),
        ("hibernated", snap.instances_hibernated),
    ] {
        line(
            &mut o,
            format_args!("isoplane_instances{{node=\"{node}\",tier=\"{tier}\"}} {v}"),
        );
    }

    histogram_lines(
        &mut o,
        node,
        "isoplane_dirty_set_bytes",
        "Dirty bytes per instance at capture",
        &snap.dirty_set_bytes,
        &[64 * 1024, 256 * 1024, 1 << 20, 4 << 20, 16 << 20, 64 << 20],
    );
    line(
        &mut o,
        format_args!("# HELP isoplane_delta_storage_bytes Hibernation delta bytes held"),
    );
    line(
        &mut o,
        format_args!("# TYPE isoplane_delta_storage_bytes gauge"),
    );
    line(
        &mut o,
        format_args!(
            "{} {}",
            n("isoplane_delta_storage_bytes"),
            snap.delta_storage_bytes
        ),
    );

    line(
        &mut o,
        format_args!(
            "# HELP isoplane_traps_total Guest faults. Traps are never retried on the same memory."
        ),
    );
    line(&mut o, format_args!("# TYPE isoplane_traps_total counter"));
    line(
        &mut o,
        format_args!("{} {}", n("isoplane_traps_total"), snap.traps_total),
    );
    line(
        &mut o,
        format_args!("# HELP isoplane_trap_rate Traps per dispatch"),
    );
    line(&mut o, format_args!("# TYPE isoplane_trap_rate gauge"));
    let denom = snap.dispatches_total.max(1) as f64;
    line(
        &mut o,
        format_args!(
            "{} {:.6}",
            n("isoplane_trap_rate"),
            snap.traps_total as f64 / denom
        ),
    );
    line(
        &mut o,
        format_args!("# HELP isoplane_hang_breakers_tripped Routes currently failing fast"),
    );
    line(
        &mut o,
        format_args!("# TYPE isoplane_hang_breakers_tripped gauge"),
    );
    line(
        &mut o,
        format_args!(
            "{} {}",
            n("isoplane_hang_breakers_tripped"),
            snap.breaker_trips
        ),
    );

    for (name, help, ty, v) in [
        (
            "isoplane_proxy_requests_total",
            "Requests accepted by the front proxy",
            "counter",
            proxy.requests_total.load(Relaxed),
        ),
        (
            "isoplane_proxy_requests_unrouted_total",
            "Requests with no matching route",
            "counter",
            proxy.requests_404.load(Relaxed),
        ),
        (
            "isoplane_proxy_requests_shed_total",
            "Requests refused at the edge before cells saturated",
            "counter",
            proxy.requests_shed.load(Relaxed),
        ),
        (
            "isoplane_proxy_requests_failed_total",
            "Dispatches that returned an error",
            "counter",
            proxy.requests_failed.load(Relaxed),
        ),
        (
            "isoplane_proxy_route_generation",
            "Route-table generation",
            "gauge",
            proxy.route_generation.load(Relaxed),
        ),
    ] {
        line(&mut o, format_args!("# HELP {name} {help}"));
        line(&mut o, format_args!("# TYPE {name} {ty}"));
        line(&mut o, format_args!("{} {}", n(name), v));
    }

    for (name, help, h) in [
        (
            "isoplane_proxy_request_latency_us",
            "End-to-end latency observed at the proxy",
            &proxy.request_latency,
        ),
        (
            "isoplane_proxy_admission_wait_us",
            "Time spent queued at admission before a slot freed",
            &proxy.admission_wait,
        ),
    ] {
        line(&mut o, format_args!("# HELP {name} {help}"));
        line(&mut o, format_args!("# TYPE {name} summary"));
        line(
            &mut o,
            format_args!(
                "{name}{{node=\"{node}\",quantile=\"0.5\"}} {:.0}",
                h.p50_us()
            ),
        );
        line(
            &mut o,
            format_args!(
                "{name}{{node=\"{node}\",quantile=\"0.99\"}} {:.0}",
                h.p99_us()
            ),
        );
        line(
            &mut o,
            format_args!("{name}_count{{node=\"{node}\"}} {}", h.count()),
        );
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
            occupancy_ms_total: 4.25,
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
    fn every_series_the_node_publishes_is_present() {
        let p = ProxyMetrics::new();
        p.requests_total.store(9, Relaxed);
        p.dispatches_tier_fast.store(4, Relaxed);
        let out = render("n1", &p, snap());
        for series in [
            "isoplane_dispatches_total",
            "isoplane_occupancy_ms_total",
            "isoplane_activation_us",
            "isoplane_wake_us",
            "isoplane_instances",
            "isoplane_dirty_set_bytes",
            "isoplane_delta_storage_bytes",
            "isoplane_trap_rate",
        ] {
            assert!(out.contains(series), "missing {series} from /metrics");
        }
        assert!(
            out.contains("tier=\"fast\"} 4"),
            "dispatches are counted per tier"
        );
    }

    #[test]
    fn quantiles_come_out_of_the_samples() {
        let out = render("n1", &ProxyMetrics::new(), snap());
        assert!(
            out.contains("isoplane_activation_us{node=\"n1\",quantile=\"0.5\"} 700"),
            "{out}"
        );
        assert!(
            out.contains("isoplane_wake_us{node=\"n1\",quantile=\"0.5\"} 9000"),
            "{out}"
        );
    }


    #[test]
    fn an_empty_node_renders_without_dividing_by_zero() {
        let out = render("n1", &ProxyMetrics::new(), NodeSnapshot::default());
        assert!(
            out.contains("isoplane_trap_rate{node=\"n1\"} 0.000000"),
            "no dispatches is a zero rate, not a NaN"
        );
        assert!(!out.contains("NaN"), "nothing renders as NaN");
    }
}
