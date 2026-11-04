# wasmtime

## Separate compiled support from guest capabilities

Apply the [dependency and capability checks](../../../AGENTS.md#dependency-management) to threads, shared memory, multi-memory, relaxed SIMD, and the exceptions proposal. A resolved Cargo graph describes what Wasmtime can support, while explicit engine configuration and validation decide what a guest may use. Construction probes and hostile-module tests exercise that runtime boundary even when another dependency compiles additional support in.

**Not this:** Trusting `default-features = false` on one crate while another crate pulls wasmtime with defaults, so that an atomics module validates because of a dependency-graph property.

**Exception:** None.

## Keep one wasmtime in the graph

Use the same engine version for pre-initialisation and execution. A template is the result of running the guest's initialiser, so an initialiser run under one engine and a guest run under another means the import surface has two linkers, the two engines' feature sets and trap behaviour can differ, and every engine upgrade is two upgrades. If a tool forces a second version, run it in its own process behind a file interface, not linked into the host build.

**Not this:** A pre-initialiser embedding one wasmtime major, with its own sealed linker and WASI emulation written against that API, beside a data plane on another major with its own.

**Exception:** None.

## Make a killed instance a state, not a panic

Represent terminal instance state explicitly and return an error for invalid subsequent operations instead of panicking.

**Not this:** `panic!` on a call into a discarded instance, in a binary built to abort on panic.

**Exception:** None.

## Pin the fork and qualify its memory patch

Pin maintained patches reproducibly and qualify them with tests at both the patched component and its integration boundary. See [fork maintenance](../../../AGENTS.md#wasmtime-fork-maintenance) for the current procedure.
