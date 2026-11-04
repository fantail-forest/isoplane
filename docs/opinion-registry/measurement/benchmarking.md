# Benchmarking

## Keep performance measurement reproducible and separate from correctness tests

Keep maintained benchmark workloads, invocation scripts and report tooling when they make measurements reproducible. Prefer maintained load generators over building our own; use an open-loop HTTP generator for platform latency under a specified arrival rate. Keep machine-dependent performance checks outside `cargo test`.

Report measurements with their workload, source revision, environment and uncertainty. A regression gate needs an owner-approved threshold, a comparable baseline, repeated measurements and an explicit policy for noise or inconclusive results. A single favorable run does not qualify performance.

**Not this:** A machine-dependent threshold in the correctness suite, a custom load framework where a maintained tool suffices, or deleting the only reproducible workload after making a performance decision.

**Exception:** A small custom generator is appropriate for an arrival process maintained tools cannot express. Microbenchmarks may exercise a named internal mechanism.

## Measure the path a customer takes

Drive the customer entry point with a stated arrival process and measure at the response. A benchmark below that boundary measures an internal component, not end-to-end platform throughput.

**Not this:** A generator that submits directly to a cell's queue and reports the result as requests per second.

**Exception:** A microbenchmark of one mechanism, named as such, quoted with the interface it drove.

## Retain instrumentation according to its ongoing value

Prefer the product's tracing mechanism for attribution. Keep expensive experimental probes disabled in production, and remove comparison implementations when the experiment ends. Retain reusable benchmark instrumentation when it supports a maintained workload; production instrumentation earns its place through operational value and acceptable measured overhead.

**Not this:** Leaving a costly experimental probe enabled on production traffic, or deleting useful measurement tooling merely because its first experiment finished.

**Exception:** A temporary probe may run in production for a specific investigation with bounded overhead and a defined removal or disablement condition.
