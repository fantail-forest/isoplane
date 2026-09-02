# Benchmarking

## Keep benchmarks in the benchmark crate and never under `cargo test`

Generate load and produce figures in `wisp-bench` and `bench/`. Move any measurement that needs a real guest, a real cell, and a quiet machine there, whether or not it has a `#[test]` attribute. Give a benchmark no pass or fail; it produces a measurement, and what to do about it is a decision a person makes reading it.

**Not this:** Density, activation-latency, and park-cost measurements in a crate's `tests/` directory, passing on every machine because their thresholds cannot fail.

**Exception:** None.

## Measure the path a customer takes

Send load in through the proxy with a stated arrival process and measure at the response. Don't describe a benchmark that calls below the proxy and the lease interface as platform throughput; it measures an internal component.

**Not this:** A generator that submits directly to a cell's queue and reports the result as requests per second.

**Exception:** A microbenchmark of one mechanism, named as such, quoted with the interface it drove.

## Keep the instrument out of the product

Keep the machinery a benchmark needs to attribute time (sample carriers, lock statistics, comparison arms) with the benchmark, on a branch or behind a feature that production builds do not enable. Ship the product with `tracing` spans and nothing else of the instrument.

**Not this:** A benchmark's timestamp instrument compiled into the cell and sampling production traffic by default.

**Exception:** None.

## A benchmark's tests test the benchmark

Unit tests of an analysis function are welcome in the benchmark crate. A "test" that runs the generator against a real cell to check that traces tile a request is a smoke run and is named as one.

**Not this:** A benchmark crate whose tests depend on the acceptance crate's statistics module and on guest fixtures built by a build script.

**Exception:** None.

## Prefer an off-the-shelf load tool

An open-loop HTTP generator with a latency histogram exists in several maintained tools. Writing one is justified only when the question cannot be asked through the proxy, and the answer to that is usually to fix the proxy.

**Not this:** A load generator with its own SKU projection, campaign planner, and report renderer.

**Exception:** A small in-tree generator for a specific arrival process the maintained tools lack, kept to what that process needs.
