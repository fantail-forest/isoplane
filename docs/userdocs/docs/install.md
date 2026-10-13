---
title: Install the SDK
---

import {OnlyOn, PlatformToggle} from '@site/src/components/Platform';

<PlatformToggle />

Install the `isoplane` CLI as a global .NET tool, then run `isoplane setup`
to check prerequisites and install project templates. Projects restore their SDK
and shared WASM runtime through NuGet. No Isoplane workload, MSI, Rust, Mono or
WASI compiler is needed. Releases support Linux x64, Windows x64 and macOS ARM64.
Package restore needs internet access.

## Prerequisite: a .NET SDK

`dotnet tool install` needs a .NET 10 SDK.

<OnlyOn platforms="linux">

Isoplane runs on x64 distributions that use glibc. Install the .NET 10 SDK
from your distribution's own packages where it has one. On Ubuntu 24.04:

```bash
sudo apt-get install -y dotnet-sdk-10.0
```

Where it has none, install Microsoft's SDK together with its runtime in a
directory of its own, such as `~/.dotnet`. A Microsoft SDK placed into a root
that holds a distribution's runtime cannot install tools: see
[troubleshooting](troubleshooting.md#dotnet-tool-install-cannot-find-dotnettoolsettingsxml).

Isoplane also loads ICU, OpenSSL 3, zlib and libstdc++ from the system, and
reads its CA certificate bundle. Ubuntu's .NET package brings all of them. On
any distribution, `isoplane setup` looks for them before it downloads anything
and names the ones that are missing.

</OnlyOn>

<OnlyOn platforms="win">

Install the .NET 10 SDK before installing the CLI. Also install the
[Visual C++ x64 Redistributable](https://aka.ms/vs/17/release/vc_redist.x64.exe);
`isoplane setup` stops and asks for it when it is missing.

</OnlyOn>

<OnlyOn platforms="mac">

Install the ARM64 .NET 10 SDK.

</OnlyOn>

Setup reuses whatever `dotnet` you have and never changes your `DOTNET_ROOT`.

## Install the CLI and the SDK

```bash
dotnet tool install --global Isoplane.Cli --prerelease
isoplane setup
```

<OnlyOn platforms="mac linux">

The tool installs into `~/.dotnet/tools`.

</OnlyOn>

<OnlyOn platforms="win">

The tool installs into `%USERPROFILE%\.dotnet\tools`.

</OnlyOn>

Add that directory to your PATH if your .NET installation has not already done
so; reopen your terminal after changing it.

Setup installs the matching `Isoplane.Templates` package through NuGet. The CLI
package already contains its native host library.

<OnlyOn platforms="mac linux">

Setup records template ownership under `~/.isoplane`.

</OnlyOn>

<OnlyOn platforms="win">

Setup records template ownership under `%USERPROFILE%\.isoplane`.

</OnlyOn>

`ISOPLANE_HOME` overrides that location. Your shell configuration and .NET SDK
installation stay unchanged.

```bash
isoplane doctor
dotnet new list
```

Doctor checks the selected .NET SDK, native host library and templates; it reports
problems and does not repair them. The template list includes
`isoplane-web`, `isoplane-webapi` and `isoplane-handler`. Continue with [creating an application](create-an-application.md).

Setup does not report success while anything is missing.

<OnlyOn platforms="linux">

When setup finds a system library missing on Ubuntu 24.04, it shows the one
`sudo apt-get install` command that provides it and asks before running it;
`isoplane setup --yes` runs it without asking. On any other distribution it
names what is missing and stops: install your distribution's packages for
those libraries, then run `isoplane setup` again.

</OnlyOn>

For a release supplied as a directory instead of downloaded, run
`isoplane setup --source RELEASE_DIRECTORY`, and install the CLI with
`--add-source RELEASE_DIRECTORY/packages`. An HTTP release directory URL is also
accepted. This explicit mode verifies the matching release identity and package
checksums, copies packages into `ISOPLANE_HOME/packages`, and registers a local
NuGet feed. It does not make dependency restore fully offline.

## Update

If upgrading from a preview that installed an Isoplane workload or MSI, run
`isoplane uninstall-sdk` with that old CLI first. The new CLI does not manage
those older installation records.

Stop running development sessions, then update the CLI and run setup again:

```bash
dotnet tool update --global Isoplane.Cli --prerelease
isoplane setup
isoplane doctor
```

Setup updates the template registration. Existing applications retain their
pinned SDK packages in NuGet's standard cache.

The new template selects the matching SDK automatically. To move an existing
application to the new release, change the version in
`<Project Sdk="Isoplane.AspNetCore/VERSION">`, or `Isoplane.Sdk/VERSION` for an
application without ASP.NET, and in any explicit Isoplane package references, including `Isoplane.Testing` in test projects, then rebuild and
test each configuration you use.

## Repair

Run `isoplane setup` again to restore missing template registration. For a damaged
CLI or bundled native library, uninstall and reinstall the `Isoplane.Cli` tool.
Explicit `--source` setup also restores missing or damaged local feed packages.
Setup does not rewrite project package references.

## Remove the SDK

```bash
isoplane uninstall-sdk
```

This removes the resources setup owns, as recorded in `installation.json`
under the installation home: the template registration and, when installed with `--source`, the local
package feed and its NuGet registration. Resources that
existed before setup, your projects, and the CLI's own configuration and
sign-in state stay. Anything that could not be removed remains listed in the
manifest with the reason, and the command can be run again. Without a
manifest, or with a damaged one, the command explains and removes nothing.

The CLI itself remains so setup can be run again; remove it with
`dotnet tool uninstall --global Isoplane.Cli`.
