# Quoting Measurements

## Measurements inform decisions; they are not properties of the system

A benchmark result describes one run on one machine at one commit. It exists to choose between alternatives, and once the choice is made the number has done its job. Treating it as a characteristic of the platform turns a snapshot into a promise nobody made.

**Not this:** A guest README that is a measurement report from one afternoon, read later as the system's characteristics.

**Exception:** None.

## Prefer a comparison under a described load to an absolute

When a measurement is worth quoting, say what was compared, under what load, and by how much: "we benchmarked X against Y under a fan-out load of three futures per dispatch and X was five times faster." A ratio survives hardware changes and engine upgrades; an absolute figure with a CPU model attached ages quickly and is easily read as a guarantee.

**Not this:** "26.6 ns at eight tenants" in a doc comment; "18 of 30 calibrations on an EPYC 7773X" justifying a constant.

**Exception:** A figure stable enough to stand on its own may be quoted as one, with the conditions it was taken under. A value that is a fact of the platform is derived in code, not measured.

## Quote where the decision is non-obvious

Most code needs no measurement to explain it. Quote one when a reader would otherwise reasonably choose differently: a constant that looks arbitrary, an alternative that looks simpler and was tried, a default that runs against the grain. Elsewhere it is usually noise.

**Not this:** Every tunable carrying a paragraph of benchmark history.

**Exception:** None.

## Keep a measurement with the decision it informed

The place to quote a result is the commit message or the comment on the constant it justified. Treat benchmark run records as build outputs and keep them out of the repository. A directory of results invites findings that outlive the decisions they supported and get read as documentation.

**Not this:** A checked-in tree of run reports; a measurement library that tests report through.

**Exception:** A commit message may carry the run's parameters and raw figures when the decision would be unreadable without them.

## State the question before the run

A benchmark answers one question, written down first. Runs that go looking for a number to report produce numbers that mean whatever the reader wants, and reports that grow tables to hold them.

**Not this:** A campaign that produces two dozen report tables and a hard-coded SKU projection.

**Exception:** None.
