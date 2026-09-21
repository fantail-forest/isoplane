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
dotnet pack tools/Isoplane.Cli -o /tmp/isoplane-preview-feed
dotnet tool install Isoplane.Cli --version "$(scripts/release-version.py show)" --tool-path /tmp/isoplane-tools --add-source /tmp/isoplane-preview-feed
/tmp/isoplane-tools/isoplane --version
```

Every first-party package packs as the release version `Directory.Build.props`
declares. `isoplane --version` reports the installed CLI version and its source
revision.

The tool requires .NET 10 to bootstrap. `isoplane setup` downloads the release
the CLI was built from (`isoplane-linux-x64-VERSION/` under the origin named by
`ISOPLANE_RELEASE_ORIGIN`, default `https://releases.isoplane.dev/`) or reads it
from `--source RELEASE_DIRECTORY`, verifies each component against
`release.json`, and caches release packages under `ISOPLANE_HOME` (default
`~/.isoplane`). It registers the NuGet feed, installs the Isoplane workload in
the existing .NET SDK and records owned resources in `installation.json`.
Workload installation supplies the templates and build tools. Setup requests
`sudo` access when the SDK directory requires it. Missing Ubuntu native
prerequisites are installed through the `apt-get` command setup shows, after
confirmation or with `--yes`.

`isoplane doctor` verifies the installation and runs the compiler, wasm-tools
and runtime. `isoplane uninstall-sdk` removes owned resources and keeps the CLI.

`scripts/package-toolchain.py` assembles workload packs from the runtime feed,
compiler, wasi-sdk, wasm-tools, CLI and native support library, plus dependency
notices. The build uses the SDK pinned by `global.json` to obtain the stock WASI
build targets and repackages them under Isoplane-owned identities. Customer
installation uses the customer's existing .NET SDK and preserves shell SDK
selection. No Rust compiler or host Mono is required.

Run a generated app with `/tmp/isoplane-tools/isoplane dev --project /path/to/app`.
Publication follows the module path in `isoplane.toml`. Source edits rebuild into a
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

`isoplane bundle --project /path/to/app --configuration IsoplaneNativeAotRelease
--output application.tar` compiles managed deployment input through the installed
SDK. The project configuration selects execution mode. The archive contains
application assemblies, their managed dependencies and available portable symbols;
embedded resources and generated application code remain in those assemblies.
Runtime exports and platform implementation assemblies are supplied by the server.
This command needs no login and does not build WASM. External assets and satellite
assemblies currently produce an explicit unsupported-input diagnostic.

