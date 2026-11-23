# `guests/`

The guest modules the host tests drive. The Rust guests
are a Cargo workspace of their own, targeting `wasm32-unknown-unknown` under
a size-tuned release profile; the Rust guest SDK in `../guest-sdks/rust` is a
member of it. The C# guests reference the SDK in `../guest-sdks/dotnet`.

## Building

```sh
task fixtures:rust    # .build/wasm32/rust-guests/*.wasm
task fixtures:dotnet  # .build/clr-anycpu/probes/*.tar
```

The test task prepares its fixtures before running the suite. To skip the release installation journeys:

```sh
task test -- -- --skip a_prepared_release_
```

Tests read those artifacts through `isoplane-fixtures`; they do not invoke builds.
`guests/build.sh` writes `.build/wasm32/rust-guests/manifest` after validating each
module with threads, shared memory, atomics, relaxed SIMD and exceptions disabled.

Each Rust module declares one linear memory with a 128 MiB maximum,
set by `--max-memory` in `guests/.cargo/config.toml`; Rust's linker
emits no maximum by default. The same file exports `__stack_pointer`, because the
validator rejects a mutable global the module does not export.

## The Rust guests

| Module | Purpose |
| --- | --- |
| `hello` | Minimal HTTP handler fixture returning a successful response. |
| `quotes-api` | `Handler` fixture for host futures, response streaming and template initialization. |
| `misbehaver` | Hostile execution fixtures: loops, sync-over-async, allocation pressure and explicit traps. |
| `undeclared-import` | Validator fixtures importing an undeclared field or a foreign module. |


## The C# guests

| Directory   | What it is                                                                                         |
| ----------- | -------------------------------------------------------------------------------------------------- |
| `dotnet/`   | Managed behavior probes packaged for the shared runtime. See `dotnet/README.md`.                             |
