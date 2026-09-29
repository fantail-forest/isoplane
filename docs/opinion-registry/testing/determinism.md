# Determinism

## Inject time; never observe it

Give a test that needs time a clock, and advance it. Wall-clock reads in a test make the assertion depend on the machine, the load, and the scheduler, and every such test either flakes or has its threshold widened until it proves nothing. The store's tests with a manual clock are the reference pattern.

**Not this:** Sleep for a while, then assert the metered CPU was small.

**Exception:** A benchmark, which does not live under `cargo test` and is not a test.

## Assert order with sequencing, not with a race that usually resolves one way

If the behaviour under test is "B is not blocked by A", make A's blocking explicit (a gate the test holds), observe B complete while A is held, then release A. A timing bound on B is a probability, not a proof.

**Not this:** Start a spinning tenant, dispatch to another tenant, assert the second completes within some milliseconds.

**Exception:** None.

## Tests assert logic; benchmarks measure

A test passes or fails on a logical assertion about behaviour: this request got this status, this instance refused this call, this delta restored these bytes. Machine-dependent performance checks belong in a separate benchmark workflow, with the [baseline and noise requirements](../measurement/benchmarking.md#keep-performance-measurement-reproducible-and-separate-from-correctness-tests). A timing threshold in the correctness suite can turn machine load into a false behavior failure.

**Not this:** A "conformance" test asserting p90 overshoot on thirty samples; a density test asserting a slope from `smaps_rollup`; a scenario runner that renders a verdict from a figure.

**Exception:** Deterministic quantities and configured limits are logical assertions: encoded byte sizes for fixed inputs, resource accounting, size-limit rejection, and deadlines advanced with a controlled clock. Derive their expected behaviour from the acceptance-criteria rule in [AGENTS.md](../../../AGENTS.md#principles).

## Don't test the fake

If the only thing a test exercises is a fake the test file constructed, the test proves the fake. A fake stands in for a boundary (filesystem, network, clock) so the real code above it can be exercised.

**Not this:** Driving the meter with zero CPU and asserting the counter did not move; constructing an error value and pattern-matching it; asserting one enum variant differs from another.

**Exception:** None.

## Hold the race still in concurrency tests

Control every participant in a test of concurrent behaviour (single-writer, coalescing, shed-or-answer) with explicit synchronisation so the interleaving under test is the interleaving that runs. A thousand rounds of a free race is a lottery ticket, not evidence.

**Not this:** Two thousand rounds of racing dispatches with a statistical assertion on the outcome distribution.

**Exception:** Property-based tests with a seeded generator and a shrinker are deterministic and welcome.
