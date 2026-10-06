# `guests/`

The guest modules the host tests drive, and the C# samples. The Rust guests
are a Cargo workspace of their own, targeting `wasm32-unknown-unknown` under
a size-tuned release profile; the Rust guest SDK in `../guest-sdks/rust` is a
member of it. The C# guests reference the SDK in `../guest-sdks/dotnet`.

## Building

```sh
scripts/dev guests/build.sh                       # Rust guests -> .build/wasm32/rust-guests/*.wasm
guests/build-dotnet.sh                # managed probes -> .build/clr-anycpu/probes/*.tar
scripts/dev guests/build.sh --if-stale            # only when a source is newer
guests/build-dotnet.sh --if-stale
```

Task prepares fixtures before running tests:

```sh
task fixtures
task test -- -- --skip a_prepared_release_installs_builds_applications_and_uninstalls
```

Tests read those artifacts through `isoplane-fixtures`; they do not invoke builds.
`guests/build.sh` writes `.build/wasm32/rust-guests/manifest` after validating each
module with threads, shared memory, atomics, relaxed SIMD and exceptions disabled.

Each Rust module declares one linear memory with a 128 MiB maximum, set by `--max-memory` in `guests/.cargo/config.toml`; Rust's linker
emits no maximum by default.

## The Rust guests

| Module | Purpose |
| --- | --- |
| `hello` | Minimal HTTP handler fixture returning a successful response. |
| `quotes-api` | `Handler` fixture for host futures, response streaming and initialization units. |
| `misbehaver` | Hostile execution fixtures: loops, sync-over-async, allocation pressure and explicit traps. |
| `impure-cctor` | Initialization-unit fixture for bounded deferral and retry. |
| `bigheap` | Dirties a chosen number of pages and parks for local snapshot measurements. |
| `undeclared-import` | Validator fixtures importing an undeclared field or a foreign module. |


## The C# guests

| Directory   | What it is                                                                                         |
| ----------- | -------------------------------------------------------------------------------------------------- |
| `dotnet/`   | Managed behavior probes packaged for the shared runtime. See `dotnet/README.md`.                             |
