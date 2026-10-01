# Isoplane CLI

The package includes the managed CLI and `isoplane-native` support library.
Native code validates modules and runs local dev/debug sessions through the host;
project discovery, builds, watching and output are managed.
Builds still launch .NET, the supplied LLVM/compiler tools and wasm-tools.
Node operators use the daemon's HTTP administration API.

The .NET tool owns publication and source watching; its bundled native runtime
validates, prepares and executes WASM. Release builds share Linux-built managed assemblies and attach native support
for Linux x64, Windows x64 and macOS ARM64.

From the repository root, build and pack into a local feed:

```sh
git submodule update --init tools/debugger-libs
scripts/dev dotnet pack tools/Isoplane.Cli -o .isoplane-dev/preview-feed
scripts/dev dotnet tool install Isoplane.Cli --version "$(scripts/release-version.py show)" --tool-path .isoplane-dev/cli --add-source .isoplane-dev/preview-feed
.isoplane-dev/cli/isoplane --version
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

`isoplane doctor` verifies the installed SDK and native runtime.
`isoplane uninstall-sdk` removes owned resources and keeps the CLI.

`task release` packages prepared managed tools and the platform's native library
with dependency notices. Linux and macOS use workload packs; Windows uses an MSI.
Customer installation uses the customer's existing .NET SDK. No Rust compiler,
WASI compiler or host Mono is required to build managed application packages.

Run a generated app with `.isoplane-dev/cli/isoplane dev --project /path/to/app`.
Project-generated caches, preparation state, and staging files live under the
project's `.isoplane/` directory. Standard NuGet and SourceLink caches remain shared.
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

`isoplane bundle --project /path/to/app --configuration Release
--output application.tar` compiles managed deployment input through the installed
SDK. The project configuration selects execution mode. The archive contains
application assemblies, their managed dependencies and available portable symbols;
embedded resources and generated application code remain in those assemblies.
Runtime exports and platform implementation assemblies are supplied by the server.
This command needs no login and does not build WASM. External assets and satellite
assemblies currently produce an explicit unsupported-input diagnostic.

For an HTTPS endpoint using a private CA, set `SSL_CERT_FILE` to a PEM certificate
bundle. CLI downloads and telemetry use that bundle as their trust roots
on every platform, while still verifying certificate validity and hostnames.
Without it, the operating system trust configuration applies.

