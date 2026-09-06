---
id: quickstart
title: Quickstart
sidebar_position: 1
---

# Quickstart

Run an application locally with `wisp dev`. It builds the project,
validates and pre-initializes its WASM, then serves HTTP on loopback.

## Build the CLI

From the repository root, install the native development tools and build:

```bash
scripts/setup
scripts/dev cargo build -p wisp-cli
```

Rust is needed to build the CLI from this checkout. Building a C# guest also
requires the .NET SDK, the pinned NativeAOT-LLVM compiler, wasi-sdk and
wasm-tools. The pinned compiler currently runs on Linux. On macOS, build WASM
in a local Linux environment and run the resulting module natively.

## Rebuild on changes

Source changes trigger a rebuild. The running app remains available while
building; a failed build or rejected module leaves the last working app
available and prints a diagnostic. Successful reloads start fresh guest
memory.

Build output goes to `.wisp/build.log`. Runtime logs stream to stderr as JSON;
`RUST_LOG` controls their filter.

## Use your own project

`wisp dev` reads `wisp.toml` from the current directory, or the directory given
by `--watch`. It requires explicit `name`, `tenant` and `module` values.
Module paths are relative to that manifest. Existing app settings, including
handler lifecycle and limits, are read from the same manifest.

Use `[dev].build` or `--build` for a project build command. Without either,
`build.sh` is discovered in the project directory; if no build command exists,
the configured module must already exist. `--no-build` skips build commands.
For managed guests, `[dev].init_sandbox = "runtime-bringup"` selects the runtime
initialization policy explicitly; `--init-sandbox` overrides it.

The watcher compares file contents. It ignores dotfiles and directories named
`target`, `node_modules`, `bin`, `obj` and `out`, but always watches the configured
module even inside an ignored directory. Dependencies outside the project
require a rebuild that updates that module. `--no-watch` serves just the initial
build. `wisp dev --help` lists the available options.

## Scope of local testing

Local development runs the guest engine, host bindings, validator and
pre-initializer in a local process.

## Where next

- [Your first C# guest](first-csharp-guest.md).
- [The four invariants](../explanation/invariants.md) for the execution model.
