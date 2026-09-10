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

Rust is needed to build the CLI from this checkout. C# development requires
the .NET 10 SDK. On macOS, install and start Docker Desktop or a compatible
local Docker runtime. The SDK automatically provisions a pinned Linux compiler
image on first publish; it runs and reloads the resulting guest natively.
On Linux, install wasi-sdk 29 and wasm-tools and set `WASI_SDK_PATH`.

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

## Optional Mono publication on Linux

ASP.NET projects using `Wisp.AspNetCore` can explicitly publish managed IL
with Mono instead of the default NativeAOT profile:

```bash
dotnet publish -c Release -p:WispGuestRuntime=Mono
```

The installed .NET CLI selects the same profile with
`isoplane dev --guest-runtime mono`. Omitting the option honors the project
property; without that property, publication uses NativeAOT.

This profile requires the pinned .NET SDK from `global.json`, the
`wasi-experimental` workload containing Mono WASI runtime 10.0.11, wasi-sdk
29.0 through `WASI_SDK_PATH`, and `wasm-tools` on `PATH`. The current setup
script and compiler image do not install this optional workload. The build
checks the workload target and native SDK before linking; a different
workload is rejected rather than silently adapting it.

The installed-template HTTP and ordinary `Task.Delay` workflow is tested on
Linux. This does not establish macOS or Windows publication, or full cryptography
compatibility.

## Debug one local request

On Linux, the installed .NET CLI can start a fresh Mono debug session:

```bash
isoplane dev --debug --no-watch
```

This requires the Mono publication prerequisites above and an external
Mono-compatible DAP adapter. The tested adapter is Microsoft `mono-debug`
0.16.3 running under Ubuntu Mono 6.8.0.105+dfsg-3.6ubuntu2, with its framework
assemblies and configuration. The adapter also requires `netstandard.dll`
from the pinned .NET SDK's
`Microsoft/Microsoft.NET.Build.Extensions/net461/lib/` directory beside
`mono-debug.exe`; the unmodified VSIX alone is insufficient. The CLI does not
redistribute this adapter, facade, or host runtime. Attach the adapter to the printed loopback debugger endpoint, then request the printed
HTTP URL. Guest initialization starts with that request, so an adapter attach
can wait until the request arrives.

The session serves one request and exits after its response. Breakpoints,
local inspection, and stepping across an ordinary incomplete `Task.Delay`
are tested against the actual HTTP listener. Debug output and matching symbols
live in a separate private session directory and are removed when the CLI
exits; ordinary publication output is preserved. NativeAOT and source watching
are rejected with `--debug`.

Use Ctrl-C in the CLI to stop a paused guest and its owned child processes.
Reliable adapter detach while stopped is not supported. The loopback debugger
relay assumes a trusted local developer machine; it does not authenticate
other local processes.

The installed debugger acceptance tests require `WISP_DEBUG_ADAPTER_LAUNCHER`
to name an executable external adapter launcher and `WISP_DAP_PYTHON` to name
a Python interpreter with `debugpy` for DAP framing. They do not download or
redistribute the adapter.

## Scope of local testing

Local development runs the guest engine, host bindings, validator and
pre-initializer in a local process.

## Where next

- [Your first C# guest](first-csharp-guest.md).
- [The four invariants](../explanation/invariants.md) for the execution model.
