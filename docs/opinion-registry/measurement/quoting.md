# Quoting Measurements

## Measurements inform decisions; they are not properties of the system

A benchmark result describes one run on one machine at one commit. It can inform a choice between alternatives or serve as a regression baseline under comparable conditions. Treating it as an unconditional characteristic of the platform turns a snapshot into a promise nobody made.

**Not this:** A guest README that is a measurement report from one afternoon, read later as the system's characteristics.

**Exception:** None.

## Prefer a comparison under a described load to an absolute

When a measurement is worth quoting, say what was compared, under what load, and by how much: "we benchmarked X against Y under a fan-out load of three futures per dispatch and X was five times faster." Both ratios and absolute figures depend on hardware, workload and software versions. Record those conditions and the source revision; neither form guarantees the result survives an upgrade.

**Not this:** "26.6 ns at eight tenants" in a doc comment; "18 of 30 calibrations on an EPYC 7773X" justifying a constant.

**Exception:** A figure stable enough to stand on its own may be quoted as one, with the conditions it was taken under. A value that is a fact of the platform is derived in code, not measured.

## Quote where the decision is non-obvious

Most code needs no measurement to explain it. Quote one when a reader would otherwise reasonably choose differently: a constant that looks arbitrary, an alternative that looks simpler and was tried, a default that runs against the grain. Elsewhere it is usually noise.

**Not this:** Every tunable carrying a paragraph of benchmark history.

**Exception:** None.

## Keep a measurement with the decision it informed

Keep a lasting rationale in the commit message or beside the decision it explains. Put current-problem experiment reports and interpretation in repository Discussions, following the [documentation lifecycle opinion](../workflow/documentation-lifecycle.md). Treat bulk raw run records as build outputs. Keep maintained workloads, reproduction instructions and deliberately selected regression baselines with their tooling; record each baseline's provenance and comparison conditions. Separate these inputs from historical reports that could be mistaken for current performance.

**Not this:** An unmaintained tree of run reports presented as current platform characteristics, or correctness tests routed through a performance-reporting framework.

**Exception:** A commit message may carry the run's parameters and raw figures when the decision would be unreadable without them.

## State the question before the run

A benchmark answers one question, written down first. Runs that go looking for a number to report produce numbers that mean whatever the reader wants, and reports that grow tables to hold them.

**Not this:** A campaign that produces two dozen report tables and a hard-coded SKU projection.

**Exception:** None.
