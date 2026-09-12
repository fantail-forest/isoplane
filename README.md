# WISP

WISP hosts customer-written .NET applications as WebAssembly guests. The
isolation boundary is the WASM sandbox, not the CLR and not a hypervisor.
Guests are pre-initialised at deploy time into a memory template, activate
copy-on-write from it, park with an empty stack while waiting on host I/O, and
can be hibernated as a delta against the template and woken later. The host is
Rust; the guest SDK is C#.

[`AGENTS.md`](AGENTS.md) is the engineering handbook: architecture, tooling,
the rules code is held to, and the four invariants every host mechanism
derives from. [Workload security and isolation](docs/userdocs/docs/explanation/workload-security.md)
explains the current enforcement boundaries and limitations.
[`docs/userdocs/`](docs/userdocs/) is the documentation site.

## Building and testing

`scripts/dev` runs commands directly on the host, using your environment and
adding `.wisp-dev/tools/bin` to `PATH`. Install Rust, Python 3 and your
platform's C compiler first. Node.js is needed for frontend and docs builds.

```sh
scripts/setup                             # Rust guest tools
scripts/dev cargo build --workspace --all-targets
scripts/dev cargo test --workspace
scripts/dev                               # interactive development shell
```

C# builds require the .NET 10 SDK. On macOS, start Docker Desktop or a
compatible local Docker runtime; the SDK's publish integration automatically
provisions and uses a pinned Linux compiler image. The host and guest
execution remain native:

```sh
scripts/dev guests/build-dotnet.sh --if-stale
scripts/dev cargo test --workspace
```

The optional repository toolchain image remains available through
`scripts/dev --container --build` for running Linux toolchain commands.

See [the development tools guide](docs/userdocs/docs/how-to/use-development-tools.md)
for native commands and the optional container workflow.

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
| `scripts/`            | `scripts/dev` runs a local command; `scripts/clean` reclaims build intermediates. |
