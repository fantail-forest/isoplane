# Benchmarking

## A benchmark is a run of a load tool against a node, never a test

Generate load with a maintained open-loop HTTP generator against a running `isoplaned`, and read its latency histogram. Nothing in the repository generates load or renders a report, and nothing under `cargo test` measures: a measurement with a pass mark painted on is green on one machine and red on another. Give a benchmark no verdict; what to do about its figure is a decision a person makes reading it.

**Not this:** Density, activation-latency, and park-cost measurements in a crate's `tests/` directory, passing on every machine because their thresholds cannot fail. A load generator of our own with a SKU projection, a campaign planner, and a report renderer.

**Exception:** A small in-tree generator for a specific arrival process the maintained tools lack, kept to what that process needs, and kept out of `cargo test`.

## Measure the path a customer takes

Send load in through the proxy with a stated arrival process and measure at the response. Don't describe a benchmark that calls below the proxy and the lease interface as platform throughput; it measures an internal component.

**Not this:** A generator that submits directly to a cell's queue and reports the result as requests per second.

**Exception:** A microbenchmark of one mechanism, named as such, quoted with the interface it drove.

## Keep the instrument out of the product

Ship the product with `tracing` spans and nothing else of a benchmark's instrument. Machinery a benchmark needs to attribute time (sample carriers, lock statistics, comparison arms) lives with the benchmark, on a branch or behind a feature that production builds do not enable, and is deleted with it.

**Not this:** A benchmark's timestamp instrument compiled into the cell and sampling production traffic by default.

**Exception:** None.
