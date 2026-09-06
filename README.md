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

`scripts/dev` runs commands natively through the pinned open-source
[Sandbox Runtime](https://github.com/anthropics/sandbox-runtime): Seatbelt on
macOS and Bubblewrap on Linux. Install Rust, Node.js (20.11 or newer), Python 3,
and your platform's C compiler first; Linux also needs `bubblewrap` and `socat`.

```sh
scripts/setup                             # sandbox tool and Rust guest tools
scripts/dev cargo build --workspace --all-targets
scripts/dev cargo test --workspace
scripts/dev                               # interactive sandboxed shell
```

The pinned NativeAOT-LLVM C# compiler is published for Linux, not macOS ARM64.
Build those fixtures with the optional Docker toolchain, then run the host and
tests natively. Docker Desktop on macOS needs no Sysbox:

```sh
scripts/dev --container --build
scripts/dev --container guests/build-dotnet.sh --if-stale
scripts/dev --container --stop
scripts/dev cargo test --workspace
```

The sandbox uses a separate home and caches in `.wisp-dev`, permits local TCP
and package downloads, and does not inherit arbitrary shell credentials. It
imposes no CPU affinity or minimum core count. See [the sandbox guide](docs/userdocs/docs/how-to/use-the-sandbox.md)
for manual runs, network access, and the optional container workflow.

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
| `scripts/`            | `scripts/dev` sandboxes a local command; `scripts/clean` reclaims build intermediates. |
