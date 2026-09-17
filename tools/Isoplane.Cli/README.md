# Isoplane CLI

The package includes the managed CLI and `isoplane-native` support library.
Native code validates modules and runs local dev/debug sessions through the host;
project discovery, builds, watching and output are managed.
Builds still launch .NET, the supplied LLVM/compiler tools and wasm-tools.
Node operators use the daemon's HTTP administration API.

The .NET tool owns publication and source watching; its bundled native runtime
validates, prepares and executes WASM. This package currently targets the Linux
host used to build it. macOS and Windows packages are not validated.

From the repository root, build and pack into a local feed:

```sh
dotnet pack tools/Isoplane.Cli -o /tmp/wisp-preview-feed
dotnet tool install Isoplane.Cli --version 0.1.0 --tool-path /tmp/isoplane-tools --add-source /tmp/wisp-preview-feed
/tmp/isoplane-tools/isoplane --version
```

Use the version of the package you built when installing. `isoplane --version`
reports the installed CLI version and its source revision when available.

The tool requires .NET 10 to bootstrap. Install a prepared local toolchain with
`isoplane setup --source /path/to/toolchain-bundle`; `isoplane doctor` verifies
its contents and runs the SDK, compiler, wasm-tools and runtime to check native
dependencies. `ISOPLANE_HOME` overrides the default `~/.isoplane` installation.
Setup can be rerun to repair damaged files. Doctor does not repair them.

The bundle is assembled by `scripts/package-toolchain.py --help` from verified
runtime packs, wasi-sdk, wasm-tools, the managed CLI and native support library,
collected notices, and the official Microsoft Linux SDK matching `global.json`.
Assembly provisions the WASI workload pinned by that SDK's shipped manifests.
`scripts/toolchain-notices.py --help` collects dependency attribution from the
locked Cargo graphs and matching upstream sources. The
customer needs the resulting bundle and NuGet feed, not those source inputs.
No Rust compiler or host Mono is required. CLI-owned builds select the installed
toolchain automatically. For ordinary `dotnet` commands, set `DOTNET_ROOT` to
`$ISOPLANE_HOME/dotnet` and put that directory first on PATH, using
`$HOME/.isoplane/dotnet` when `ISOPLANE_HOME` is unset. This selects the same
SDK and workload even when another .NET installation is on the machine.

Run a generated app with `/tmp/isoplane-tools/isoplane dev --project /path/to/app`.
Publication follows the module path in `wisp.toml`. Source edits rebuild into a
staging directory and replace the module after successful publication; failed
builds leave the last working app serving. Ctrl+C stops the session and its
active child processes. `isoplane doctor path/to/module.wasm` runs the native
validator.

Normal development output shows compact application logs and keeps runtime
internals hidden. For process-owning test tools, `dev --json` writes JSON
ready/reloaded records with the actual bound `url`; structured runtime
diagnostics go to stderr. `RUST_LOG=debug` increases their detail. `--port 0`
requests an available port. Opt-in `--shutdown-on-stdin-close` ends the session
when its caller closes stdin; interactive sessions retain normal Ctrl+C behavior.

`isoplane bundle --project /path/to/app --configuration WispNativeAotRelease
--output application.tar` compiles managed deployment input through the installed
SDK. The project configuration selects execution mode. The archive contains
application assemblies, their managed dependencies and available portable symbols;
embedded resources and generated application code remain in those assemblies.
Runtime exports and platform implementation assemblies are supplied by the server.
This command needs no login and does not build WASM. External assets and satellite
assemblies currently produce an explicit unsupported-input diagnostic.

