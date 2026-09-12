# Isoplane Linux developer preview

This bundle targets Linux x64 on Ubuntu 24.04. It includes the pinned .NET SDK,
WASI workload, source-built guest runtimes, compiler tools and local engine.
The OS needs `libicu74`, `libssl3`, `zlib1g`, `libstdc++6`, `libzstd1`, `libtinfo6` and `ca-certificates`.
No preinstalled .NET SDK, Rust, Wasmtime or Mono is needed.

All Isoplane packages in this release use the version recorded in
`package-version.txt`. Run these commands from this extracted directory:

```sh
export ISOPLANE_HOME="${ISOPLANE_HOME:-$HOME/.isoplane}"
export DOTNET_ROOT="$PWD/toolchain/dotnet"
export PATH="$DOTNET_ROOT:$PATH"
dotnet tool install Isoplane.Cli --version @PACKAGE_VERSION@ --tool-path "$ISOPLANE_HOME/bin" --add-source "$PWD/packages"
"$ISOPLANE_HOME/bin/isoplane" setup --source "$PWD/toolchain"
export DOTNET_ROOT="$ISOPLANE_HOME/dotnet"
export PATH="$DOTNET_ROOT:$ISOPLANE_HOME/bin:$PATH"
isoplane doctor
dotnet nuget add source "$PWD/packages" --name isoplane-preview
dotnet new install "$PWD/packages/Wisp.Templates.@PACKAGE_VERSION@.nupkg"
```

For each new shell, set `ISOPLANE_HOME`, `DOTNET_ROOT` and `PATH` to the installed
paths above. Keep `packages/` available while developing: NuGet restores SDK
packages from that directory. Package restore can also contact nuget.org and
the .NET experimental compiler feed; fully offline builds are not promised.

For a new CLI version, replace `dotnet tool install` with `dotnet tool update`
and use its exact version. Run setup with the new bundle, then doctor. Setup
reuses a verified installation or repairs damaged files; doctor only diagnoses.
Before installing an updated template package, run
`dotnet new uninstall Wisp.Templates` to remove its earlier template registration. Existing projects
keep their pinned SDK and test-package versions; update those references to
this release's `package-version.txt` value when upgrading an application.
Use `dotnet nuget update source isoplane-preview --source "$PWD/packages"` when
moving the local feed or switching releases.

Follow the customer documentation for project creation, `dotnet run`,
`dotnet watch`, testing and database configuration. Debugger support is not
established by successful installation.

Third-party attribution and license texts are in `toolchain/notices/`, with
additional upstream notices retained in the .NET SDK and runtime packs.
