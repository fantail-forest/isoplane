---
id: cli-other
title: Other tools and scripts
sidebar_position: 7
---

# Other tools and scripts

## `scripts/dev`

Runs any command inside `wisp-dev:latest` under `--runtime=sysbox-runc`. It
is a convenience — the image carries the toolchain — not something the code
requires; every command below also runs wherever that toolchain is installed.

```
scripts/dev <command>...
scripts/dev --stop | --reap | --status
```

| Env | Default | Meaning |
|---|---|---|
| `WISP_IMAGE` | `wisp-dev:latest` | Image to run |
| `WISP_CONTAINER_NAME` | `wisp-<uid>-shared` | Container name. Use a distinct one for long runs. |
| `WISP_PORTS` | none | Space-separated `-p` mappings, e.g. `"8787:8787 8788:8788"`. Fixed at container creation. |
| `WISP_RUN_ARGS` | none | Extra `docker run` flags, word-split |
| `WISP_PROBE_TIMEOUT` | `15` | Seconds to wait for a container to answer a probe |
| `WISP_DOCKER_TIMEOUT` | `60` | Seconds to wait for the Docker daemon |
| `WISP_START_WAIT` | `30` | Seconds to wait for a container someone else is starting |

Inside the container: repo at `/wisp`, `CARGO_TARGET_DIR=/ctarget`,
`CARGO_HOME=/opt/cargo`, `NUGET_PACKAGES=/nuget`. Binaries land in
`/ctarget/release/`, **not** `./target/release/`.

**`WISP_*` variables are forwarded into the container**, less the seven that
configure the container itself (listed above). `docker exec` inherits nothing
from the calling shell, so without this the flavour switches the guest builders
document — `WISP_OUT=… scripts/dev guests/dotnet/build.sh` — set the variable
on the host process while the build inside used its default: a build that
succeeds and writes the wrong file, which is how a flavour comparison ends up
comparing a module against itself. The container's own control variables are
excluded so a nested `scripts/dev` cannot inherit a name or image it was not
asked for.

| Exit | Meaning |
|---|---|
| `69` | The Docker daemon did not answer |
| `70` | The container is running but did not answer a probe — refusing to start another behind a hung one |
| `71` | Refusing to remove a container that is not ours |
| `73`/`74` | Docker would not say what state the container is in, or it is paused |
| `75` | Gave up after five attempts |

## `guests/build.sh`

Builds the selected Rust guests to `guests/out/<hyphenated-crate-name>.wasm`
and runs the configured `wasm-tools` checks. It honours `CARGO_TARGET_DIR`
and removes each module it owns before rebuilding it.

## `guests/dotnet/build.sh`

Builds the C# guest to `guests/dotnet/out/<name>.wasm`. wasi-sdk 29.0 ships in
the image with `WASI_SDK_PATH` preset, and the script **refuses with the
rebuild command** if it is absent rather than trying to install it — the
container runs as your uid and `/opt` is root-owned, so a self-install could
only ever fail confusingly. NuGet packages live in the `wisp-nuget` volume.

| Env / flag | Meaning |
|---|---|
| `WISP_OUT=<basename>` | Output filename, so flavours can coexist under `out/` and be compared |
| `WISP_OUT_DIR=<dir>` | Output directory. |
| `WASI_SDK_VERSION` | Default `29.0` — the version the ILCompiler package expects |
| `WASI_SDK_PATH` | Default `/opt/wasi-sdk-<version>-x86_64-linux` |
| `-p:WispMemoryMaxBytes=N` | Declared memory maximum. `67108864` sets a 64 MiB ceiling. |
| `-p:WispCompatProbe=true` | Build the compatibility probe: LINQ, source-generated `Regex`, source-generated `System.Text.Json` |
| `-p:WispEntropyProbe=true` | Build the clone-uniqueness probe |
| `-p:NativeDebugSymbols=true` | Keep symbol names, for finding a stray import |

Any argument after the script name is passed through to `dotnet publish`.

The build **fails** if the module imports anything outside module `wisp`, and
prints the commands that show you the offending symbol.

## `guests/dotnet/check.sh`

Runs the C# module under wasmtime and asserts what the ".NET as-is" claim
needs to be true.

| Option | Meaning |
|---|---|
| `--wasi=pal` (default) | Implement only the measured runtime-PAL set; the other imports trap if called |
| `--wasi=trap` | Every non-`wisp` import traps — a negative test, not an inspection |
| `--wasi=stub` | Nothing traps; enumerate which imports are reachable |
| `--density <N>` | Activate N instances and report marginal RSS |
| `--entropy-clone` | Restore one post-initialize image onto two instances and compare five sources of tenant-visible randomness |

## `wisp-validate`

Runs a module through the **real** `wisp-control::validate` and prints the
verdict. Feature-gated because `wisp-control` pulls in the pre-initializer.

```bash
scripts/dev cargo run --release --features validator --bin wisp-validate \
    --manifest-path guests/dotnet/hostcheck/Cargo.toml
```

## `scripts/clean`

Reclaims build artifacts. A Rust build tree for a workspace containing wasmtime
gets very large very fast, so this is worth running between phases of work
rather than only at the end.

```
scripts/clean          # cheap: caches and intermediates only, no rebuild forced
scripts/clean --all    # everything, including compiled output
```

If a container build wrote root-owned trees into the bind mount, it falls back
to deleting them from inside the container.
