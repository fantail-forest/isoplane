//! `/metrics`: the node's meters as Prometheus text.

use isoplane_proxy::ProxyMetrics;
use prometheus::{proto, TextEncoder};

use crate::admin::{Latency, NodeSnapshot};

fn metric(labels: &[(&str, &str)]) -> proto::Metric {
    let mut metric = proto::Metric::default();
    metric.set_label(
        labels
            .iter()
            .map(|(name, value)| {
                let mut label = proto::LabelPair::default();
                label.set_name((*name).to_owned());
                label.set_value((*value).to_owned());
                label
            })
            .collect(),
    );
    metric
}

fn family(
    name: &str,
    help: &str,
    kind: proto::MetricType,
    metrics: Vec<proto::Metric>,
) -> proto::MetricFamily {
    let mut family = proto::MetricFamily::default();
    family.set_name(name.to_owned());
    family.set_help(help.to_owned());
    family.set_field_type(kind);
    family.set_metric(metrics);
    family
}

fn counter(labels: &[(&str, &str)], value: f64) -> proto::Metric {
    let mut metric = metric(labels);
    let mut counter = proto::Counter::default();
    counter.set_value(value);
    metric.set_counter(counter);
    metric
}

fn gauge(labels: &[(&str, &str)], value: f64) -> proto::Metric {
    let mut metric = metric(labels);
    let mut gauge = proto::Gauge::default();
    gauge.set_value(value);
    metric.set_gauge(gauge);
    metric
}

fn latency_family(node: &str, name: &str, help: &str, latency: Latency) -> proto::MetricFamily {
    summary_family(
        node,
        name,
        help,
        latency.count,
        latency.sum_us,
        &[(0.5, latency.p50_us as f64), (0.99, latency.p99_us as f64)],
    )
}

fn summary_family(
    node: &str,
    name: &str,
    help: &str,
    count: u64,
    sum: f64,
    quantiles: &[(f64, f64)],
) -> proto::MetricFamily {
    let mut summary = proto::Summary::default();
    summary.set_sample_count(count);
    summary.set_sample_sum(sum);
    summary.set_quantile(
        quantiles
            .iter()
            .map(|&(quantile, value)| {
                let mut sample = proto::Quantile::default();
                sample.set_quantile(quantile);
                sample.set_value(value);
                sample
            })
            .collect(),
    );
    let mut metric = metric(&[("node", node)]);
    metric.set_summary(summary);
    family(name, help, proto::MetricType::SUMMARY, vec![metric])
}

/// Encodes one node snapshot using Prometheus text exposition version 0.0.4.
pub fn render(node: &str, proxy: &ProxyMetrics, snap: NodeSnapshot) -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let node_labels = [("node", node)];
    let mut families = vec![
        family(
            "isoplane_dispatches_total",
            "Dispatches handled, by code tier",
            proto::MetricType::COUNTER,
            vec![
                counter(
                    &[("node", node), ("tier", "fast")],
                    proxy.dispatches_tier_fast.load(Relaxed) as f64,
                ),
                counter(
                    &[("node", node), ("tier", "optimized")],
                    proxy.dispatches_tier_optimized.load(Relaxed) as f64,
                ),
            ],
        ),
        family(
            "isoplane_cell_dispatches_total",
            "Dispatches observed inside the cell",
            proto::MetricType::COUNTER,
            vec![counter(&node_labels, snap.dispatches_total as f64)],
        ),
        family(
            "isoplane_occupancy_ms_total",
            "Measured occupied-worker milliseconds over execution and reactor intervals",
            proto::MetricType::COUNTER,
            vec![counter(&node_labels, snap.occupancy_ms_total)],
        ),
        latency_family(
            node,
            "isoplane_activation_us",
            "Cold activation from a local template",
            snap.activation,
        ),
        latency_family(
            node,
            "isoplane_wake_us",
            "Hibernation wake from the instance's local snapshot reservation",
            snap.wake,
        ),
        family(
            "isoplane_instances",
            "Instances by lifecycle state",
            proto::MetricType::GAUGE,
            [
                ("hot", snap.instances_hot),
                ("parked", snap.instances_parked),
                ("hibernated", snap.instances_hibernated),
            ]
            .into_iter()
            .map(|(tier, value)| gauge(&[("node", node), ("tier", tier)], value as f64))
            .collect(),
        ),
        summary_family(
            node,
            "isoplane_dirty_set_bytes",
            "Dirty bytes per completed local capture",
            snap.dirty_set.count,
            snap.dirty_set.sum_bytes,
            &[
                (0.5, snap.dirty_set.p50_bytes as f64),
                (0.99, snap.dirty_set.p99_bytes as f64),
            ],
        ),
        family(
            "isoplane_snapshot_written_bytes_total",
            "Bytes written by completed local snapshot captures",
            proto::MetricType::COUNTER,
            vec![counter(&node_labels, snap.snapshot_written_bytes as f64)],
        ),
        family(
            "isoplane_traps_total",
            "Guest faults. Traps are never retried on the same memory.",
            proto::MetricType::COUNTER,
            vec![counter(&node_labels, snap.traps_total as f64)],
        ),
        family(
            "isoplane_trap_rate",
            "Traps per dispatch",
            proto::MetricType::GAUGE,
            vec![gauge(
                &node_labels,
                snap.traps_total as f64 / snap.dispatches_total.max(1) as f64,
            )],
        ),
        family(
            "isoplane_hang_breakers_tripped",
            "Routes currently failing fast",
            proto::MetricType::GAUGE,
            vec![gauge(&node_labels, snap.breaker_trips as f64)],
        ),
    ];
    for (name, help, value) in [
        (
            "isoplane_proxy_requests_total",
            "Requests accepted by the front proxy",
            proxy.requests_total.load(Relaxed),
        ),
        (
            "isoplane_proxy_requests_unrouted_total",
            "Requests with no matching route",
            proxy.requests_404.load(Relaxed),
        ),
        (
            "isoplane_proxy_requests_shed_total",
            "Requests refused at the edge before cells saturated",
            proxy.requests_shed.load(Relaxed),
        ),
        (
            "isoplane_proxy_requests_failed_total",
            "Dispatches that returned an error",
            proxy.requests_failed.load(Relaxed),
        ),
    ] {
        families.push(family(
            name,
            help,
            proto::MetricType::COUNTER,
            vec![counter(&node_labels, value as f64)],
        ));
    }
    families.push(family(
        "isoplane_proxy_route_generation",
        "Generation of the installed application routing snapshot",
        proto::MetricType::GAUGE,
        vec![gauge(
            &node_labels,
            proxy.route_generation.load(Relaxed) as f64,
        )],
    ));
    for (name, help, histogram) in [
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
        let mut summary = proto::Summary::default();
        summary.set_sample_count(histogram.count());
        summary.set_sample_sum(histogram.sum_us() as f64);
        summary.set_quantile(
            [(0.5, histogram.p50_us()), (0.99, histogram.p99_us())]
                .into_iter()
                .map(|(quantile, value)| {
                    let mut sample = proto::Quantile::default();
                    sample.set_quantile(quantile);
                    sample.set_value(value);
                    sample
                })
                .collect(),
        );
        let mut metric = metric(&node_labels);
        metric.set_summary(summary);
        families.push(family(name, help, proto::MetricType::SUMMARY, vec![metric]));
    }
    TextEncoder::new().encode_to_string(&families).expect(
        "all families above have nonempty names and samples, and String writing cannot fail",
    )
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
            dirty_set: crate::admin::SnapshotSizes {
                p50_bytes: 900 * 1024,
                p99_bytes: 3 << 20,
                count: 3,
                sum_bytes: (100 * 1024 + 900 * 1024 + (3 << 20)) as f64,
            },
            snapshot_written_bytes: 5 << 20,
            traps_total: 1,
            breaker_trips: 0,
        }
    }

    #[test]
    fn summaries_and_capture_totals_come_from_the_observations() {
        let mut snapshot = snap();
        snapshot.dirty_set = crate::admin::SnapshotSizes {
            p50_bytes: u64::MAX,
            p99_bytes: u64::MAX,
            count: 2,
            sum_bytes: u128::from(u64::MAX) as f64 * 2.0,
        };
        let proxy = ProxyMetrics::new();
        proxy.request_latency.record_us(1);
        proxy.request_latency.record_us(3);
        let text = render("n1", &proxy, snapshot);
        for sample in [
            "isoplane_activation_us_sum{node=\"n1\"} 5900",
            "isoplane_activation_us_count{node=\"n1\"} 4",
            "isoplane_wake_us_sum{node=\"n1\"} 57000",
            "isoplane_wake_us_count{node=\"n1\"} 3",
            "isoplane_proxy_request_latency_us_sum{node=\"n1\"} 4",
            "isoplane_proxy_request_latency_us_count{node=\"n1\"} 2",
            "isoplane_dirty_set_bytes_count{node=\"n1\"} 2",
            "isoplane_snapshot_written_bytes_total{node=\"n1\"} 5242880",
        ] {
            assert!(
                text.lines().any(|line| line == sample),
                "missing {sample}: {text}"
            );
        }
        assert!(
            text.lines().any(|line| line
                == format!(
                    "isoplane_dirty_set_bytes_sum{{node=\"n1\"}} {}",
                    u128::from(u64::MAX) as f64 * 2.0
                )),
            "dirty-set sums do not overflow: {text}"
        );
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
            "isoplane_snapshot_written_bytes_total",
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
            out.contains("isoplane_trap_rate{node=\"n1\"} 0"),
            "no dispatches is a zero rate, not a NaN"
        );
        assert!(!out.contains("NaN"), "nothing renders as NaN");
    }
}
