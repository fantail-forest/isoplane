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
dotnet tool install Isoplane.Cli --version "$(scripts/release-version.py show)" --tool-path /tmp/isoplane-tools --add-source /tmp/wisp-preview-feed
/tmp/isoplane-tools/isoplane --version
```

Every first-party package packs as the release version `Directory.Build.props`
declares. `isoplane --version` reports the installed CLI version and its source
revision.

The tool requires .NET 10 to bootstrap. `isoplane setup` downloads the release
the CLI was built from (`isoplane-linux-x64-VERSION/` under the origin named by
`ISOPLANE_RELEASE_ORIGIN`, default `https://releases.isoplane.dev/`) or reads it
from `--source RELEASE_DIRECTORY`, verifies each component against
`release.json`, installs the toolchain archive and package feed under
`ISOPLANE_HOME` (default `~/.isoplane`), registers the feed and template with
`dotnet`, writes `env` and the startup-file line that sources it, and records
every change in `installation.json`. Missing native prerequisites are installed
through the one `sudo apt-get` command setup shows, after confirmation or with
`--yes`. `isoplane doctor` verifies the installation and runs the SDK, compiler,
wasm-tools and runtime; it does not repair. `isoplane uninstall-sdk` removes what
the manifest records and keeps the CLI.

The bundle is assembled by `scripts/package-toolchain.py --help` from verified
runtime packs, wasi-sdk, wasm-tools, the managed CLI and native support library,
collected notices, and the official Microsoft Linux SDK matching `global.json`.
Assembly provisions the WASI workload pinned by that SDK's shipped manifests.
`scripts/toolchain-notices.py --help` collects dependency attribution from the
locked Cargo graphs and matching upstream sources. The
customer needs the resulting bundle and NuGet feed, not those source inputs.
No Rust compiler or host Mono is required. CLI-owned builds select the installed
toolchain automatically. Ordinary `dotnet` commands find the same SDK and
workload through `$ISOPLANE_HOME/env`, which puts `$ISOPLANE_HOME/dotnet` first
on PATH; `DOTNET_ROOT` is left alone.

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

