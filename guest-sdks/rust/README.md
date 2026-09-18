# `guest-sdks/rust` — the Rust Isoplane guest SDK

Internal Rust helpers for guest fixtures: async execution, typed host
capabilities, arena allocation and initialization units.

The [C# SDK](../dotnet) is the customer SDK. These Rust helpers support
fixtures, hostile guests and benchmarks, and evolve with those tests.

The [public API source](src/lib.rs) documents the SDK surface.

## Declaring an entrypoint

One macro call at the crate root, and exactly one:

```rust
isoplane_guest::export_handler!(Api);
```

`Handler` provides the HTTP entrypoint; the macro emits its host exports.

## Build and test

Guests are built by [`../../guests/build.sh`](../../guests/build.sh), which is
the single definition of how they are built; call it rather than reproducing the
cargo line.

```bash
scripts/dev guests/build.sh

# The SDK's own tests, including the assertion that its import names and order
# have not drifted from `crates/isoplane-abi`. The `json` feature is required:
# the preinit tests use `init::config_vars`, which is gated behind it.
scripts/dev bash -lc 'cd /isoplane/guests && cargo test -p isoplane-guest --features json \
    --target x86_64-unknown-linux-gnu'
```

## Workspace

The source lives here, beside the C# SDK. The *workspace* is **`guests/`** —
a separate question, and the one that decides how this crate is built.

It wants that workspace because that is where the guest-shaped settings are:
the size-tuned release profile (`opt-level = "z"`, LTO, `panic = "abort"`,
strip) that the guests linking this crate are built with, and `isoplane-abi` pinned
with `default-features = false`. The root workspace builds host crates for the
native target and has neither.

`Cargo.toml` names that root explicitly with `workspace = "../../guests"`,
because Cargo finds a root by walking up and would otherwise reach the repo
root, whose explicit `members` list does not include this crate.

`crate-type = ["rlib"]` keeps the helpers in the guest library.
`export_handler!` emits the host-facing exports into the fixture crate. Use one
entrypoint macro per fixture to avoid duplicate export names.