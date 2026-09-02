# wasmtime

## Disable engine features explicitly and assert at construction

Turn threads, shared memory, multi-memory, relaxed SIMD, and the exceptions proposal off by explicit calls on the engine config, and make the engine refuse to construct if a probe module using any of them validates. Cargo feature unification can compile support back in from another crate's dependency; the assertion is what keeps validation honest.

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
