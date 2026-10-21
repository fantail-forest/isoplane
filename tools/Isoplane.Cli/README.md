# Isoplane CLI

The package includes the managed CLI and `isoplane-native` support library.
Native code validates modules and runs local dev/debug sessions through the host;
project discovery, builds, watching and output are managed.
Application builds use the .NET SDK and restore the shared WASM runtime from NuGet.
Node operators use the daemon's HTTP administration API.

The .NET tool owns publication and source watching; its bundled native runtime
validates, prepares and executes WASM. Release builds share Linux-built managed assemblies and attach native support
for Linux x64, Windows x64 and macOS ARM64.

From the repository root, use `task cli` to stage a development CLI and
`task release` to assemble platform packages.

Every first-party package packs as the release version `Directory.Build.props`
declares. `isoplane --version` reports the installed CLI version and its source
revision.

The tool requires .NET 10. `isoplane setup` checks native prerequisites and
installs the matching `Isoplane.Templates` package through NuGet. Project restores
supply the SDK targets and shared runtime. No Isoplane workload or MSI is required.
Windows requires the Microsoft Visual C++ x64 Redistributable. On Ubuntu, setup
can install missing native prerequisites after confirmation or with `--yes`.

For an explicit `--source DIRECTORY|URL`, setup verifies `release.json` and its
package checksums, copies the packages into `ISOPLANE_HOME/packages` (default
`~/.isoplane/packages`), and registers that local NuGet feed. Setup records ownership
in `installation.json`. `isoplane uninstall-sdk` removes owned templates and
optional feed resources, keeping the CLI. `isoplane doctor` checks the SDK,
bundled native library and templates.

`task release` assembles NuGet packages with dependency notices. No Rust compiler,
WASI compiler or host Mono is required to build managed application packages.
Run a generated app with `dotnet run` or `isoplane dev --project /path/to/app`.
The application package, session state and staging files live under the
project's `.isoplane/` directory, and what a session prepares from the project
under its MSBuild intermediate output directory. `ISOPLANE_TARGET_DIR`
moves all of that to a per-project folder under the directory it names. The
compiled shared runtime is kept once per user: in `ISOPLANE_CACHE_DIR/runtimes`
when that is set, else `ISOPLANE_HOME/cache/runtimes` when `ISOPLANE_HOME` is
set, and otherwise in the platform's cache directory (`~/.cache/isoplane`,
`~/Library/Caches/isoplane` or `%LOCALAPPDATA%\Isoplane\Cache`).
`isoplane cache info` reports it and `isoplane cache clean` empties it;
`isoplane prepare-runtime --runtime DIRECTORY` fills it ahead of the first session. Set
`SOURCE_DATE_EPOCH` to date every file in an application package with that time,
so that the same source builds the same package on any machine. Standard NuGet
and SourceLink caches remain shared.
The session's port, settings and limit overrides come from the project's launch
profile and User Secrets. Source edits rebuild the application package and replace
it after a successful build; failed builds leave the last working app serving. Ctrl+C stops the session and its
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
SDK. Debug and Release select managed compilation settings. The archive contains
application assemblies, their managed dependencies and available portable symbols;
embedded resources and generated application code remain in those assemblies.
Runtime exports and platform implementation assemblies are supplied by the server.
This command needs no login and does not build WASM. Copied content assets and
satellite assemblies retain their output paths in the archive.

For an HTTPS endpoint using a private CA, set `SSL_CERT_FILE` to a PEM certificate
bundle. CLI downloads and telemetry use that bundle as their trust roots
on every platform, while still verifying certificate validity and hostnames.
Without it, the operating system trust configuration applies.

## Telemetry

A CLI built with `-p:IsoplaneTelemetryEndpoint=URL` records one event per command
under `ISOPLANE_HOME/telemetry` and sends earlier runs' events to that URL while a
later command works. A build without the property records and sends nothing, which
is every build from source. `tools/TelemetryProtocol/command-event.schema.json`
defines the event. The customer
reference page, generated from the same definitions, covers opting out and what
is collected.

The debugger includes Mono's MIT-licensed `debugger-libs` and Microsoft's
`Microsoft.VisualStudio.Shared.VSCodeDebugProtocol` library. Their license files
are included under `licenses/` in the tool package. The latter is governed by
[Microsoft's redistribution terms](https://go.microsoft.com/fwlink/?linkid=832965),
including requirements for distributor and end-user agreements.
