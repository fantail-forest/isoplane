---
title: Install the SDK
---

The Isoplane tools install in two steps: the `isoplane` command as a global
.NET tool, then `isoplane setup`, which installs the SDK, compiler tools,
runtime, package feed and project templates that match that command. You do
not need an Isoplane checkout, Rust, Mono, or a separately installed WASI SDK.

The validated platforms are Ubuntu 24.04 x64 and Windows 11 x64. Package
restore needs internet access.

## Prerequisite: a .NET SDK

`dotnet tool install` needs a .NET 10 SDK. On Ubuntu 24.04, install the packaged
one, which also brings every native library the Isoplane toolchain needs:

```bash
sudo apt-get install -y dotnet-sdk-10.0
```

On Windows, install the .NET 10 SDK before installing the CLI. Setup installs
the required Visual C++ runtime if it is missing.

Setup reuses whatever `dotnet` you have and never changes your `DOTNET_ROOT`.
On another Linux distribution, install the .NET 10 SDK its packages provide,
or Microsoft's SDK with its own runtime in its own directory; do not mix the
two in one root (see [troubleshooting](troubleshooting.md)). Setup checks the
native libraries it needs before downloading anything and names what is
missing.

## Install the CLI and the SDK

```bash
dotnet tool install --global Isoplane.Cli --prerelease
isoplane setup
```

The tool installs into `~/.dotnet/tools` on Linux and `%USERPROFILE%\.dotnet\tools`
on Windows. Add that directory to your PATH if your .NET installation has not
already done so; reopen your terminal after changing it.

Setup downloads and verifies the release matching the CLI. On Linux it installs
an Isoplane workload into your existing .NET SDK. On Windows it uses Windows
Installer to install the compiler and runtime tools, and registers the project
template. Windows installations are maintained by Isoplane setup and Windows
Installer; `dotnet workload update` does not manage them. Setup requests
administrator access when needed.

Release packages and the installation record are kept under `~/.isoplane`
on Linux and `%USERPROFILE%\.isoplane` on Windows (`ISOPLANE_HOME` overrides this).
Setup registers the release's NuGet feed and runs the installed tools before reporting success. Your shell configuration and
.NET SDK selection stay unchanged.

```bash
isoplane doctor
dotnet new list
```

Doctor checks the installation's integrity and runs the tools; it reports
problems and does not repair them. The template list includes
`isoplane-web`. Continue with [creating an application](create-an-application.md).

When setup finds a native library missing on Ubuntu 24.04, it shows the one
`sudo apt-get install` command that provides it and asks before running it;
`isoplane setup --yes` runs it without asking. Setup does not report success
while anything is missing.

For a release supplied as a directory instead of downloaded, run
`isoplane setup --source RELEASE_DIRECTORY`, and install the CLI with
`--add-source RELEASE_DIRECTORY/packages`. `ISOPLANE_RELEASE_ORIGIN` points
setup at another release origin.

## Update

Stop running development sessions, then update the CLI and run setup again:

```bash
dotnet tool update --global Isoplane.Cli --prerelease
isoplane setup
isoplane doctor
```

Setup installs the release matching the updated CLI, updates the
template registration, and keeps earlier SDK packages so existing
applications still restore. Windows SDK versions install side by side. Setup
refuses a release the installed CLI was not built from, naming the `dotnet tool update` command that installs the matching
CLI.

The new template selects the matching SDK automatically. To move an existing
application to the new release, change the version in
`<Project Sdk="Isoplane.AspNetCore/VERSION">` and in any explicit Isoplane package
references, including `Isoplane.Testing` in test projects, then rebuild and
test each configuration you use.

## Repair

Run `isoplane setup` again. It reuses verified components, downloads what is
missing or damaged, and re-registers the feed and templates. It does not rewrite
your project's package references.

## Remove the SDK

```bash
isoplane uninstall-sdk
```

This removes the resources setup owns, as recorded in `installation.json`
under the installation home: the SDK tools, the package feed and its NuGet
registration, and the template registration. Resources that
existed before setup, your projects, and the CLI's own configuration and
sign-in state stay. Anything that could not be removed remains listed in the
manifest with the reason, and the command can be run again. Without a
manifest, or with a damaged one, the command explains and removes nothing.

The CLI itself remains so setup can be run again; remove it with
`dotnet tool uninstall --global Isoplane.Cli`.
