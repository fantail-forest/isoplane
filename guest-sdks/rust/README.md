# `guest-sdks/rust` — the Rust WISP guest SDK

Internal Rust helpers for guest fixtures: async execution, typed host
capabilities, arena allocation and initialization units.

The [C# SDK](../dotnet) is the customer SDK. These Rust helpers support
fixtures, hostile guests and benchmarks, and evolve with those tests.

The [API source](src/lib.rs) documents the helper surface.

## Declaring an entrypoint

One macro call at the crate root, and exactly one:

```rust
wisp_guest::export_handler!(Api);
```

`Handler` provides the HTTP entrypoint; the macro emits its host exports.

## Build and test

Guests are built by [`../../guests/build.sh`](../../guests/build.sh), which is
the single definition of how they are built; call it rather than reproducing the
cargo line.

```bash
scripts/dev guests/build.sh

# The SDK's own tests, including the assertion that its import names and order
# have not drifted from `crates/wisp-abi`. The `json` feature is required:
# the preinit tests use `init::config_vars`, which is gated behind it.
scripts/dev bash -lc 'cd /wisp/guests && cargo test -p wisp-guest --features json \
    --target x86_64-unknown-linux-gnu'
```

## Workspace

This crate is a member of the **`guests/`** workspace, not the root one: it is
built for wasm32 under that workspace's size-tuned release profile
(`opt-level = "z"`, LTO, `panic = "abort"`), the same profile as the guests that
link it. `Cargo.toml` names that root explicitly with `workspace = "../../guests"`,
because Cargo finds a root by walking up — which from here reaches the repo
root, and that one excludes `guest-sdks`.

`crate-type = ["rlib"]` keeps the helpers in the guest library.
`export_handler!` emits the host-facing exports into the fixture crate. Use one
entrypoint macro per fixture to avoid duplicate export names.