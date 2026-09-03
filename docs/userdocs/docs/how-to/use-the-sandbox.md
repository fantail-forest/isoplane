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

The image carries Rust with the wasm targets, the .NET SDK, `wasm-tools` and
wasi-sdk, as declared in `Dockerfile`.

The build takes `HOST_UID` and `HOST_GID` build args, defaulting to 1000. They
pre-create the cache mountpoints owned by your uid, which is what lets the
container run as you rather than as root:

```bash
docker build -t wisp-dev:latest \
    --build-arg HOST_UID=$(id -u) --build-arg HOST_GID=$(id -g) .
```

## Run commands

```bash
./scripts/dev cargo test --workspace      # the full suite
./scripts/dev bash                        # an interactive shell
./scripts/dev guests/build.sh             # build the Rust guests
./scripts/dev guests/build-dotnet.sh      # build the C# guests
```

The repository is bind-mounted at `/wisp`, which is the working directory.
Three volumes carry the caches so a rebuild is not a fresh download:

| Volume | Mounted at | Env |
|---|---|---|
| `wisp-ctarget` | `/ctarget` | `CARGO_TARGET_DIR=/ctarget` |
| `wisp-cargo-registry` | `/opt/cargo/registry` | `CARGO_HOME=/opt/cargo` |
| `wisp-nuget` | `/nuget` | `NUGET_PACKAGES=/nuget` |

:::caution `./target` is not where your container build went
Inside the container, binaries are at `/ctarget/release/`, not
`./target/release/`.
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

The variables that configure the container itself (`WISP_IMAGE`,
`WISP_CONTAINER_NAME`, `WISP_PORTS`, `WISP_PROBE_TIMEOUT`,
`WISP_DOCKER_TIMEOUT`, `WISP_START_WAIT`) are not forwarded. Anything outside
the `WISP_*` prefix is not forwarded either; set it inside the command:

```bash
./scripts/dev bash -lc 'RUST_LOG=debug cargo test -p wisp-host'
```

## Manage containers

```bash
./scripts/dev --status    # what we own, and whether it responds
./scripts/dev --stop      # remove this name's container
./scripts/dev --reap      # remove all of our own stopped containers
```

Use a distinct `WISP_CONTAINER_NAME` for an isolated container, and pass the
name to every invocation, `--stop` included: bare `--stop` targets the default
shared name.

`stopped <name>` is printed whenever nothing refused the request, and "no
such container" counts as success, so the line appears for a name that never
existed. If the distinction matters, check `docker ps`.

## What it refuses

Each refusal has a distinct exit code:

| Situation | Behaviour |
|---|---|
| The Docker daemon does not answer | Refuses (exit 69) rather than queuing work behind a wedged daemon |
| The container is running but does not answer a probe | Refuses (exit 70) rather than starting another behind a hung one |
| The container exists but is not labelled `wisp.owner=<your uid>` | Refuses to touch it (exit 71) |
| Docker will not say what state the container is in | Refuses (exit 73 or 74) |
| The container is `paused` | Refuses; somebody paused it on purpose |
| The container is `created` or `restarting` | Waits; that is what the winner of a startup race looks like for a moment |
