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

The pinned experimental NativeAOT-LLVM compiler has Linux ARM64 and x64 builds,
but no macOS ARM64 build. Use the optional Linux toolchain on Docker Desktop
for C# fixtures:

```bash
scripts/dev --container --build
scripts/dev --container guests/build-dotnet.sh --if-stale
scripts/dev --container --stop
scripts/dev cargo test --workspace
```

The image follows the Docker host architecture and runs as your uid. It uses
the standard container runtime, with no Sysbox, privileged mode, or Docker
socket mounted inside it. Fixture outputs are shared through the checkout.
Native tests reuse fresh fixtures; changing C# sources requires rebuilding
them on Linux. A missing or stale fixture fails with a message naming the
required tooling, rather than skipping its tests.

A Linux installation of .NET and wasi-sdk can also build the guests directly;
set `WASI_SDK_PATH` to its installation directory.

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
system-device and scratch paths. The sandbox protects the surrounding host;
commands can modify source files in the checkout.

Package hosts used by Cargo, NuGet, and Rust tooling are allowed through SRT's
proxy. Other external destinations require `--allow-domain HOST`. Local TCP is
allowed for integration tests and manual work; local services remain reachable
on macOS. Unix sockets, including the Docker socket and SSH agent, are not
allowed. This is development isolation, separate from WISP's WASM guest boundary.

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
