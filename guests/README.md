# `guests/`

The Rust guest fixtures form a separate Cargo workspace targeting
`wasm32-unknown-unknown`, with a size-tuned release profile. Internal Rust
helpers supply async execution, capability bindings and arena allocation.

## Building

Use [the guest build script](build.sh) for the fixture set at this revision.
The SDK unit tests use the native target so the executor can be exercised
without a WebAssembly engine.

```bash
scripts/dev guests/build.sh          # -> guests/out/*.wasm, sizes, import/export dump
scripts/dev bash -lc 'cd guests && cargo test -p wisp-guest'
```

## Parking and wake-up

The executor polls ready tasks and returns to the host when no task is ready.
A host-future wake starts another guest call on a fresh stack. Async locals
remain in guest memory; a live native stack is not a checkpoint. The host owns
I/O resources and their lifetimes while the guest is parked.

The [workspace manifest](Cargo.toml) identifies the internal Rust helper crate.
Local snapshot support needs defined preservation, rebinding or loss behavior
for each host resource; returning from a slice does not make resources portable.

## HTTP handlers and capabilities

`Handler::new` constructs per-instance state. `Handler::handle` receives a
request and context and returns an async response. `export_handler!` connects
that handler to the host exports. `Ctx::spawn` schedules background work that
can continue after a response while the host drives its futures.

Use [the contract definitions](../crates/wisp-abi/src/imports.rs) for interfaces.
The module imports determine which declared host capabilities it can request;
validation and linking enforce those declarations.

## Memory and initialization

The Rust arena allocator keeps live object addresses fixed. Snapshot hints
zero free regions and can reclaim a free tail; they do not shrink WebAssembly
linear memory.

Rust fixtures enumerate initialization units explicitly. A preparation attempt
may defer a forbidden unit and retry within the host's bound. Discarding guest
state does not undo earlier allowed external effects. Captured guest memory
retains values produced during allowed initialization.

## Fixtures

Each fixture has a focused crate directory. Host tests exercise the relevant
runtime behavior or validator rejection; the build script selects the modules
for this revision. Fixture names and source are the maintained inventory.
