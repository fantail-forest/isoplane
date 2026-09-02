---
id: use-the-sandbox
title: Use the sandbox
sidebar_position: 1
---

# Use the sandbox


`scripts/dev` runs commands in the repository toolchain container. Native
commands can also run with the corresponding tools installed. This container
is separate from the WebAssembly guest isolation boundary.

## Build the image

```bash
docker build -t wisp-dev:latest .
```

The image carries Rust (with the wasm targets), .NET 10, `wasm-tools` and
wasi-sdk 29.0, as declared in `Dockerfile`. The container runs as your uid; `/opt` is
root-owned.

The build takes `HOST_UID` / `HOST_GID` build args, defaulting to 1000. They
pre-create the cache mountpoints owned by your uid, which is what makes
`--user $(id -u):$(id -g)` work: Docker creates a volume's mountpoint
`root:root` when the path does not exist in the image, and sysbox then
re-chowns volume contents into the container's uid range, so a container
running as you cannot write to them. Without this you get "Permission denied"
on `.cargo-build-lock`, which reads like a cargo bug.

```bash
docker build -t wisp-dev:latest \
    --build-arg HOST_UID=$(id -u) --build-arg HOST_GID=$(id -g) .
```

## Run commands

```bash
./scripts/dev cargo test --workspace      # the full suite
./scripts/dev bash                        # an interactive shell
./scripts/dev guests/build.sh             # build the Rust guests
./scripts/dev guests/dotnet/build.sh      # build the C# guest
```

The repository is bind-mounted at `/wisp` and the container's working
directory is `/wisp`. Three volumes carry the caches so a rebuild is not a
fresh download:

| Volume | Mounted at | Env |
|---|---|---|
| `wisp-ctarget` | `/ctarget` | `CARGO_TARGET_DIR=/ctarget` |
| `wisp-cargo-registry` | `/opt/cargo/registry` | `CARGO_HOME=/opt/cargo` |
| `wisp-nuget` | `/nuget` | `NUGET_PACKAGES=/nuget` |

:::caution `./target` is not where your container build went
`scripts/dev` sets `CARGO_TARGET_DIR=/ctarget` so a container build never
writes into the bind-mounted repo. Inside the sandbox, binaries are at
`/ctarget/release/`, not `./target/release/`.
:::

## Expose ports

Set `WISP_PORTS` before creating a container, or stop its existing container
first. This opens an interactive shell with the selected port mappings:

```bash
./scripts/dev --stop
WISP_PORTS="8787:8787 8788:8788" ./scripts/dev bash
```

## Pass environment through

`WISP_*` variables set on your shell are forwarded into the container:

```bash
WISP_OUT=wisp_guest_dotnet_compat ./scripts/dev guests/dotnet/build.sh -p:WispCompatProbe=true
```

`docker exec` inherits nothing from the calling shell, so this is explicit
forwarding rather than something that happens for free. The six variables
that configure the container itself — `WISP_IMAGE`, `WISP_CONTAINER_NAME`,
`WISP_PORTS`, `WISP_PROBE_TIMEOUT`, `WISP_DOCKER_TIMEOUT`, `WISP_START_WAIT` —
are excluded, so a nested `scripts/dev` cannot inherit a
name or an image it was not asked for.

Anything outside the `WISP_*` prefix is not forwarded; set it inside the
command instead:

```bash
./scripts/dev bash -lc 'RUST_LOG=debug cargo test -p wisp-host'
```

## Manage containers

```bash
./scripts/dev --status    # what we own, and whether it responds
./scripts/dev --stop      # remove this name's container
./scripts/dev --reap      # remove ALL of our own *stopped* containers
```

Use a distinct `WISP_CONTAINER_NAME` for a separate command session. Pass
the name to every invocation, `--stop` included: bare `--stop` targets the
default shared name, so a session that ran under a custom name and then stopped
without it aims at the wrong container and leaves its own running.

And `stopped <name>` is not a receipt. It is printed whenever nothing refused
the request, and "no such container" counts as success — so the line appears
verbatim for a name that never existed. If the distinction matters, check
`docker ps` rather than believing the echo.

## The rules it enforces for you

`scripts/dev` refuses rather than guesses, and each refusal has a distinct
exit code. Understanding them saves diagnosis time:

| Situation | Behaviour |
|---|---|
| The Docker daemon does not answer | Refuses (exit 69). It does not queue work behind a wedged daemon. |
| The container is running but does not answer a probe | Refuses (exit 70) rather than starting another behind a hung one. |
| The container exists but is not labelled `wisp.owner=<your uid>` | Refuses to touch it (exit 71). There are unrelated containers on shared machines. |
| Docker will not say what state the container is in | Refuses (exit 73/74). An unknown state is a stop, never an absence. |
| The container is `paused` | Refuses. Somebody paused it on purpose. |
| The container is `created` or `restarting` | **Waits.** That is what the winner of a startup race looks like for a moment. |

Nothing runs as root: a container writing as root into the bind mount leaves
trees you cannot even unlink, because sysbox's userns remap lands them on a
high-numbered uid.
