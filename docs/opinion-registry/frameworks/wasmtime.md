# wasmtime

## Separate compiled support from guest capabilities

Apply the [dependency and capability checks](../../../AGENTS.md#dependency-management) to threads, shared memory, multi-memory, relaxed SIMD, and the exceptions proposal. A resolved Cargo graph describes what Wasmtime can support, while explicit engine configuration and validation decide what a guest may use. Construction probes and hostile-module tests exercise that runtime boundary even when another dependency compiles additional support in.

**Not this:** Trusting `default-features = false` on one crate while another crate pulls wasmtime with defaults, so that an atomics module validates because of a dependency-graph property.

**Exception:** None.

## Keep one wasmtime in the graph

Use the same engine version for pre-initialisation and execution. A template is the result of running the guest's initialiser, so an initialiser run under one engine and a guest run under another means the import surface has two linkers, the two engines' feature sets and trap behaviour can differ, and every engine upgrade is two upgrades. If a tool forces a second version, run it in its own process behind a file interface; never link it into the host build.

**Not this:** A pre-initialiser embedding one wasmtime major, with its own sealed linker and WASI emulation written against that API, beside a data plane on another major with its own.

**Exception:** None.

## Make a tombstone a state, not a panic

After a hard kill, move the instance to a state in which every further call returns an error. The engine does not poison a store after a trap, so the host must, and it must do so without taking the process down.

**Not this:** `panic!` on a call into a tombstoned instance, in a binary built to abort on panic.

**Exception:** None.

## Pin the private fork and qualify its memory patch

The root manifest pins a full commit from `fantail-forest/wisp-wasmtime`, based
on a stable upstream release. Builders need read access to that private repository.
The engine derives Wasmtime's serialized-module version from this pin, so changing
the fork revision invalidates prepared artifacts and compilation caches.

Keep memory mapping, allocator reset and failure-path tests in the Wasmtime fork.
Keep preparation, activation and hibernation integration tests in WISP. For a fork
update, run `cargo test -p wasmtime --lib runtime::vm::cow` and
`cargo test -p wasmtime --test cow_memory_image` in the fork, then the affected
WISP suites against the new pin. Preserve upstream release history and keep WISP
patches as separate commits.

**Not this:** An untracked patch or path dependency outside the checkout, or a
fork update that reuses artifacts identified only by the upstream version.

**Exception:** None.
