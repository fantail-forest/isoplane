---
id: use-development-tools
title: Use development tools
sidebar_position: 1
---

# Use development tools

`scripts/dev` runs commands directly on the host from the repository root.
It inherits your environment and adds `.wisp-dev/tools/bin` to `PATH`.
With no command, it opens your shell.

Install Rust, Python 3 and your platform's C compiler, then run:

```bash
scripts/setup
scripts/dev cargo build --workspace --all-targets
scripts/dev cargo test --workspace
```

Setup installs Rust WASM targets and the pinned `wasm-tools` executable.
C# guest builds also need the pinned .NET SDK, the source-built runtime feed
and WASI compiler tools described in [Build a C# guest](build-a-csharp-guest.md).
Node.js is needed for frontend and documentation builds.

```bash
scripts/dev
scripts/dev cargo run -p wispd -- --config path/to/wispd.toml
scripts/dev cargo run -p wisp-cli -- --help
```

## Optional Linux toolchain container

```bash
scripts/dev --container --build
scripts/dev --container cargo test --workspace
scripts/dev --container --status
scripts/dev --container --stop
```

`WISP_CONTAINER_NAME` selects a container; use the same name when stopping it.
The script only removes containers labelled with your uid. Stop containers
when finished. Set `WISP_PORTS` before creating a container to publish ports;
`WISP_IMAGE` selects its image. Container build outputs are in `/ctarget`.
