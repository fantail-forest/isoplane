---
id: use-the-sandbox
title: Use the sandbox
sidebar_position: 1
---

# Use the sandbox

`scripts/dev` uses the pinned open-source [Sandbox Runtime](https://github.com/anthropics/sandbox-runtime)
(SRT) to run local commands with filesystem and network restrictions. It uses
Seatbelt on macOS and Bubblewrap on Linux. The runtime is a research preview;
its version and dependency lockfile live in `scripts/`.

## Set up native tools

Install Rust, Node.js 20.11 or newer, Python 3, and the platform C compiler
(Xcode Command Line Tools on macOS). On Linux install `bubblewrap` and `socat`.
Then run:

```bash
scripts/setup
scripts/dev cargo build --workspace --all-targets
scripts/dev cargo test --workspace
```

Setup installs the pinned SRT dependency, Rust WASM targets, and `wasm-tools`.
Missing or unusable sandbox support fails the command; it never falls back to
an unsandboxed run. Tests and builds can still be invoked directly on a host
with the required tools.

## Build C# guests

Install the .NET 10 SDK. On macOS, start Docker Desktop or a compatible local
Docker runtime. The SDK invokes a pinned Linux compiler through the selected
local Docker socket; guest execution stays native:

```bash
scripts/dev guests/build-dotnet.sh --if-stale
scripts/dev cargo test --workspace
```

Tests build missing or stale fixtures on demand through the shared fixture
helper. No separate container prebuild is needed. Compiler containers are
removed after each publish; `.wisp-dev/compiler` retains packages for later
builds. The optional repository toolchain image remains available through
`scripts/dev --container --build` for other Linux toolchain work.

On Linux, install .NET, wasi-sdk and `wasm-tools`; set `WASI_SDK_PATH` to the
wasi-sdk installation directory. C# publication uses the native toolchain.

## Run and test manually

```bash
scripts/dev                              # interactive shell
scripts/dev cargo run -p wispd -- --help
scripts/dev cargo run -p wispd -- --check --config path/to/wispd.toml
scripts/dev cargo run -p wisp-cli -- --help
scripts/dev --allow-domain api.example.com cargo test -p wisp-host
```

Commands start in the checkout. Native listeners are reachable on localhost;
bind development servers to loopback. On Linux, SRT's network namespace keeps
listeners inside the sandbox: use clients in the same sandbox shell.

The launcher imposes no CPU affinity, CPU quota, or minimum core count. The
host's execution and reactor pools use ordinary OS scheduling. Linux operators
can apply `taskset` or service-manager CPU policies explicitly when running
production or benchmarking workloads outside this development launcher.

## Files and environment

The project is writable except for Git and editor configuration directories.
Other home-directory contents are hidden except for the selected Rust, .NET,
and Node tool installations. `.wisp-dev` holds the sandbox home, caches, guest
tools and temporary files; it is ignored by Git. SRT also permits its standard
system-device and scratch paths. On macOS, CoreCLR can also use its fixed
`/tmp/.dotnet` named-mutex directory, and native tools can use the OS per-user
temporary directory. Commands can modify source files in the checkout.

Package hosts used by Cargo, NuGet, and Rust tooling are allowed through SRT's
proxy. Other external destinations require `--allow-domain HOST`. Local TCP is
allowed for integration tests and manual work; local services remain reachable
on macOS. The selected local Docker Unix socket is allowed on macOS; other
private Unix sockets, including the SSH agent, remain blocked. The launcher
resolves `DOCKER_CONTEXT` or `DOCKER_HOST` before entering the sandbox and
forwards only the resolved endpoint, without Docker credentials. Package TLS
verification can use the macOS `trustd.agent` service.

Docker access permits indirect host filesystem access through container mounts.
The remaining restrictions provide protection against accidental direct file
operations; they do not contain code deliberately using Docker to bypass them.
This development sandbox is separate from WISP's WASM guest boundary.

The launcher forwards `WISP_*`, `WASI_SDK_PATH`, `CARGO_BUILD_JOBS`, and terminal
locale settings. Other variables can be set explicitly inside the sandbox:

```bash
scripts/dev env RUST_LOG=debug cargo test -p wisp-host
```

Run the real sandbox checks outside an existing OS sandbox:

```bash
python3 -m unittest discover -s scripts/tests
```

macOS rejects nesting Seatbelt sandboxes. An editor or agent already running
inside one must launch this workflow through its host-execution permission.

## Optional container lifecycle

```bash
scripts/dev --container --status
scripts/dev --container --stop
scripts/dev --container --reap
```

`WISP_CONTAINER_NAME` selects a container; use the same name when stopping it.
The script only removes containers labelled with your uid, and refuses to
queue work behind an unresponsive container. Stop containers when finished.
Set `WISP_PORTS` before creating a container to publish manual-testing ports.
`WISP_IMAGE` selects its image. Container build outputs are in `/ctarget`;
native outputs are in the checkout's `target` directory.
