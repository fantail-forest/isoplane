# WISP

WISP hosts customer-written .NET applications as WebAssembly guests. The
isolation boundary is the WASM sandbox, not the CLR and not a hypervisor.
Guests are pre-initialised at deploy time into a memory template, activate
copy-on-write from it, park with an empty stack while waiting on host I/O, and
can be hibernated as a delta against the template and woken later. The host is
Rust; the guest SDK is C#.

[`AGENTS.md`](AGENTS.md) is the engineering handbook: architecture, tooling,
the rules code is held to, and the four invariants every host mechanism
derives from. [`docs/userdocs/`](docs/userdocs/) is the documentation site.

## Building and testing

The whole toolchain (Rust with the wasm targets, the .NET SDK, `wasm-tools`,
wasi-sdk) ships in one container image. A checkout with those tools installed
locally needs no container; the crates, tests and scripts never require one.

```sh
docker build -t wisp-dev:latest .          # the toolchain image, once
scripts/dev cargo test --workspace         # the full suite, inside it
scripts/dev bash                           # a shell inside it
scripts/dev --stop                         # remove your container when done

cargo build --workspace --all-targets      # on a host with the toolchain
cargo test --workspace
```

Tests that need a guest module build it on demand through the `wisp-fixtures`
crate, which runs `guests/build.sh` and `guests/build-dotnet.sh` and fails
naming the missing tool when the toolchain is absent.

## Layout

The host crates live under `crates/`; each manifest carries a `description`,
and that is the crate inventory:

```sh
cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | "\(.name): \(.description)"'
```

| Path                  | What it is                                                                |
| --------------------- | ------------------------------------------------------------------------- |
| `crates/`              | The host: the contract, engine, local snapshots, proxy, cell, control plane, `wispd` and the `wisp` command. |
| `guest-sdks/dotnet`    | The C# guest SDK. |
| `guest-sdks/rust`      | Internal Rust helpers for guest test fixtures. |
| `guests/`             | The guest modules: fixtures the host tests drive, and the C# samples.     |
| `docs/userdocs`       | The Docusaurus documentation site.                                        |
| `docs/opinion-registry` | How software is built here.                                             |
| `scripts/`            | `scripts/dev` runs a command in the toolchain container; `scripts/clean` reclaims build intermediates. |
