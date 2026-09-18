---
title: Install on Linux
---

Use the supplied preview release directory. It contains `toolchain/`, `packages/`,
`samples/`, a `README.md`, `package-version.txt` naming the one version every
package in the release carries, and `release.json`, which records the source
revision the release was built from and the checksum of each component. You do
not need a WISP checkout, Rust, Mono, or a separately installed WASI SDK.

The current package is for Linux x64 with glibc. The validated baseline is
Ubuntu 24.04 with `libicu74`, `libssl3`, `zlib1g`, `libstdc++6`,
`libzstd1`, `libtinfo6`, and `ca-certificates` installed. Alpine and other architectures need different
packages. Package restore needs internet access.

## Install the CLI and toolchain

In a terminal, change into the supplied release directory:

```bash
preview_release="$PWD"
export ISOPLANE_HOME="${ISOPLANE_HOME:-$HOME/.isoplane}"
preview_version=$(cat "$preview_release/package-version.txt")
export DOTNET_ROOT="$preview_release/toolchain/dotnet"
export PATH="$DOTNET_ROOT:$HOME/.dotnet/tools:$PATH"
dotnet tool install --global Isoplane.Cli --version "$preview_version" \
  --add-source "$preview_release/packages"
isoplane setup --source "$preview_release"
export DOTNET_ROOT="$ISOPLANE_HOME/dotnet"
export PATH="$DOTNET_ROOT:$PATH"
isoplane doctor
```

The CLI installs as a global .NET tool in `~/.dotnet/tools`. Setup checks that
the release is the one the installed CLI was built from, then installs its SDK,
compiler tools and runtime in your user directory. Doctor checks their
integrity and runs the tools to detect missing native libraries. It reports
problems; it does not repair them.

Keep the installed tools available in new terminals by adding these lines to
your shell's startup file:

```bash
export ISOPLANE_HOME="${ISOPLANE_HOME:-$HOME/.isoplane}"
export DOTNET_ROOT="$ISOPLANE_HOME/dotnet"
export PATH="$DOTNET_ROOT:$HOME/.dotnet/tools:$PATH"
```

## Install the project template

```bash
dotnet nuget add source "$preview_release/packages" --name isoplane-preview
dotnet new install "$preview_release/packages/Wisp.Templates.$preview_version.nupkg"
```

Continue with [creating an application](create-an-application.md). Keep the
release directory while using its local package feed.

## Update or repair

Stop running development sessions before updating. Change into the new release
directory, read its version, then update the CLI and toolchain:

```bash
preview_release="$PWD"
preview_version=$(cat "$preview_release/package-version.txt")
dotnet tool update --global Isoplane.Cli --version "$preview_version" \
  --add-source "$preview_release/packages"
isoplane setup --source "$preview_release"
isoplane doctor
dotnet nuget update source isoplane-preview --source "$preview_release/packages"
```

Setup refuses a release that the installed CLI was not built from, naming the
`dotnet tool update` command that installs the matching CLI. Run the two
commands in the order above.

Replace the installed template package so the old and new versions do not
compete for the same template name:

```bash
dotnet new uninstall Wisp.Templates
dotnet new install "$preview_release/packages/Wisp.Templates.$preview_version.nupkg"
```

The new template selects the matching SDK automatically. Existing applications
need an update too:

- Change the version in `<Project Sdk="Wisp.AspNetCore/VERSION">` to the value
  in `package-version.txt`. Update any explicit Wisp package references to that
  version as well.
- In each application with a local `nuget.config`, update its feed:
  `dotnet nuget update source isoplane-preview --source "$preview_release/packages" --configfile nuget.config`.
- In each test project, run:

```bash
dotnet add package Isoplane.Testing --version "$preview_version" \
  --source "$preview_release/packages"
```

Rebuild and test each application configuration you use. These releases have
unique package versions; there is no need to clear the global NuGet cache.

For repair without an upgrade, rerun setup from the same complete release and run
doctor again. Setup repairs damaged toolchain files; it does not rewrite your
project's package references.
