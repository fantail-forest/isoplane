//! `metrics.md` — from the exposition [`wispd::metrics::render`] actually
//! produces.
//!
//! Not from a list of series names kept beside it: the generator asks the real
//! exporter to render a node, then reads its own `# HELP` and `# TYPE` lines
//! and its label sets back out. A series that is exported is documented, a
//! series that is documented is exported, and the help text on the page is the
//! help text on the wire — because they are the same string.
//!
//! The section headings are the one editorial decision, and [`SECTIONS`] must
//! *partition* the exposition: a new series belongs to exactly one section, and
//! adding one without saying which fails the docs build.

use std::collections::BTreeMap;

use anyhow::Result;
use wisp_proxy::ProxyMetrics;
use wispd::admin::NodeSnapshot;

use crate::table::{cell, Table};
use crate::Block;

const PAGE: &str = "metrics.md";

/// Node label used only for the render the generator reads. It never reaches
/// the page: the label *name* does, the value does not.
const SAMPLE_NODE: &str = "node-name";

/// The page's sections, in page order, and the series each covers.
const SECTIONS: &[(&str, &[&str])] = &[
    (
        "metrics-dispatch",
        &[
            "wisp_dispatches_total",
            "wisp_cell_dispatches_total",
            "wisp_cpu_ms_total",
        ],
    ),
    (
        "metrics-lifecycle",
        &[
            "wisp_instances",
            "wisp_dirty_set_bytes",
            "wisp_delta_storage_bytes",
        ],
    ),
    (
        "metrics-latency",
        &[
            "wisp_activation_us",
            "wisp_wake_us",
            "wisp_proxy_request_latency_us",
            "wisp_proxy_admission_wait_us",
        ],
    ),
    (
        "metrics-faults",
        &[
            "wisp_traps_total",
            "wisp_trap_rate",
            "wisp_hang_breakers_tripped",
        ],
    ),
    (
        "metrics-proxy",
        &[
            "wisp_proxy_requests_total",
            "wisp_proxy_requests_unrouted_total",
            "wisp_proxy_requests_shed_total",
            "wisp_proxy_requests_failed_total",
            "wisp_proxy_route_generation",
        ],
    ),
];

/// One series, as the exposition describes itself.
#[derive(Debug, Clone, Default)]
struct Series {
    ty: String,
    help: String,
    /// Label names other than `node`, with every value seen, in first-seen
    /// order. `tier="fast"|"optimized"` is a fact about the exposition, so it
    /// is read out of the exposition.
    labels: Vec<(String, Vec<String>)>,
}

/// Parses a Prometheus text exposition into `name -> Series`.
fn parse(exposition: &str) -> BTreeMap<String, Series> {
    let mut out: BTreeMap<String, Series> = BTreeMap::new();
    for line in exposition.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            if let Some((name, help)) = rest.split_once(' ') {
                out.entry(name.to_string()).or_default().help = help.to_string();
            }
        } else if let Some(rest) = line.strip_prefix("# TYPE ") {
            if let Some((name, ty)) = rest.split_once(' ') {
                out.entry(name.to_string()).or_default().ty = ty.to_string();
            }
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            let Some((head, _)) = line.split_once(' ') else { continue };
            let (base, labels) = match head.split_once('{') {
                Some((b, l)) => (b, l.trim_end_matches('}')),
                None => (head, ""),
            };
            // A summary's quantiles and a histogram's buckets are suffixed
            // series of the same metric, not metrics of their own.
            let name = base
                .trim_end_matches("_bucket")
                .trim_end_matches("_sum")
                .trim_end_matches("_count");
            let Some(s) = out.get_mut(name) else { continue };
            for kv in labels.split(',').filter(|k| !k.is_empty()) {
                let Some((k, v)) = kv.split_once('=') else { continue };
                let v = v.trim_matches('"');
                // `node` is on every series; the page says so once in prose.
                // `quantile` and `le` are the shape of a summary/histogram,
                // not a dimension a reader queries by.
                if k == "node" || k == "quantile" || k == "le" {
                    continue;
                }
                match s.labels.iter_mut().find(|(n, _)| n == k) {
                    Some((_, vals)) => {
                        if !vals.iter().any(|x| x == v) {
                            vals.push(v.to_string());
                        }
                    }
                    None => s.labels.push((k.to_string(), vec![v.to_string()])),
                }
            }
        }
    }
    out
}

/// Renders one node's meters, with a snapshot chosen so every series appears.
///
/// `NodeSnapshot::default()` alone would leave the dirty-set histogram with no
/// samples, which still emits its `# TYPE` line — but a `tier="optimized"`
/// dispatch counter that has never been incremented emits nothing to read a
/// label off. So the counters are nudged: the *values* are discarded, only the
/// shape is read.
fn exposition() -> String {
    use std::sync::atomic::Ordering::Relaxed;
    let p = ProxyMetrics::new();
    p.dispatches_tier_fast.store(1, Relaxed);
    p.dispatches_tier_optimized.store(1, Relaxed);
    wispd::metrics::render(SAMPLE_NODE, &p, NodeSnapshot::default())
}

/// Requires every exported series to appear in exactly one documented section.
fn check_partition(series: &BTreeMap<String, Series>) -> Result<()> {
    for (id, names) in SECTIONS {
        for n in *names {
            if !series.contains_key(*n) {
                anyhow::bail!(
                    "metrics.md: section {id} documents `{n}`, which /metrics does not export. \
                     Delete the row, or fix the name."
                );
            }
        }
    }
    for name in series.keys() {
        let n = SECTIONS.iter().filter(|(_, names)| names.contains(&name.as_str())).count();
        if n != 1 {
            anyhow::bail!(
                "metrics.md: `{name}` is exported by /metrics but appears in {n} documented \
                 sections, expected exactly 1. Add it to SECTIONS in crates/wisp-docs — an \
                 exported series nobody documented is a meter nobody can use."
            );
        }
    }
    Ok(())
}

/// Generates reference blocks from the node metric exposition.
pub fn blocks() -> Result<Vec<Block>> {
    let series = parse(&exposition());
    check_partition(&series)?;

    let mut out = vec![Block::new(PAGE, "metrics-preamble", preamble(&series))];
    for (id, names) in SECTIONS {
        out.push(Block::new(PAGE, id, section(&series, names)));
    }
    Ok(out)
}

fn preamble(series: &BTreeMap<String, Series>) -> String {
    let summaries = series.values().filter(|s| s.ty == "summary").count();
    let histograms = series.values().filter(|s| s.ty == "histogram").count();
    format!(
        "`GET /metrics` on the admin listener, in Prometheus text exposition format. Every series \
         carries a `node=\"<name>\"` label. There are **{}** series: {}, {}, {} and {}. The tables \
         below, including the help text, are generated from what the exporter emits.",
        series.len(),
        plural(series.values().filter(|s| s.ty == "counter").count(), "counter"),
        plural(series.values().filter(|s| s.ty == "gauge").count(), "gauge"),
        plural(summaries, "summary"),
        plural(histograms, "histogram"),
    )
}

/// `1 histogram`, not `1 histograms`. A generated sentence still has to read
/// like a sentence.
fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        return format!("{n} {word}");
    }
    match word {
        "summary" => format!("{n} summaries"),
        w => format!("{n} {w}s"),
    }
}

fn section(series: &BTreeMap<String, Series>, names: &[&str]) -> String {
    let mut t = Table::new(&["Series", "Type", "Labels", "Meaning"]);
    for n in names {
        let s = &series[*n];
        let labels = if s.labels.is_empty() {
            String::new()
        } else {
            s.labels
                .iter()
                .map(|(k, vs)| {
                    let vals: Vec<String> = vs.iter().map(|v| format!("`\"{v}\"`")).collect();
                    format!("`{k}` = {}", vals.join(" \\| "))
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        t.row(&[format!("`{n}`"), s.ty.clone(), labels, cell(&s.help)]);
    }
    t.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_help_column_is_the_help_the_exporter_emits() {
        let s = parse(&exposition());
        let cpu = &s["wisp_cpu_ms_total"];
        assert_eq!(cpu.ty, "counter");
        assert!(exposition().contains(&format!("# HELP wisp_cpu_ms_total {}", cpu.help)));
    }

    #[test]
    fn label_values_are_read_off_the_exposition() {
        let s = parse(&exposition());
        assert_eq!(
            s["wisp_dispatches_total"].labels,
            vec![("tier".to_string(), vec!["fast".to_string(), "optimized".to_string()])]
        );
        assert_eq!(
            s["wisp_instances"].labels[0].1,
            vec!["hot", "parked", "hibernated"]
        );
        // `node` is on everything and is stated once in prose instead.
        assert!(s["wisp_cpu_ms_total"].labels.is_empty());
    }

    /// The guard, proven to fire in both directions.
    #[test]
    fn a_series_in_no_section_and_a_section_with_no_series_are_both_refused() {
        // A node that exports something nobody documented.
        let mut extra = parse(&exposition());
        extra.insert(
            "wisp_brand_new_meter".into(),
            Series { ty: "gauge".into(), help: "new".into(), labels: vec![] },
        );
        let err = check_partition(&extra).unwrap_err().to_string();
        assert!(err.contains("wisp_brand_new_meter"), "{err}");
        assert!(err.contains("nobody documented is a meter nobody can use"), "{err}");

        // A page that documents something the node stopped exporting.
        let mut gone = parse(&exposition());
        gone.remove("wisp_cpu_ms_total");
        let err = check_partition(&gone).unwrap_err().to_string();
        assert!(err.contains("wisp_cpu_ms_total"), "{err}");
        assert!(err.contains("does not export"), "{err}");
    }

    #[test]
    fn every_exported_series_is_in_exactly_one_section() {
        blocks().unwrap();
    }
}
