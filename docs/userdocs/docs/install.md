---
title: Install on Linux
---

The Isoplane tools install in two steps: the `isoplane` command as a global
.NET tool, then `isoplane setup`, which installs the SDK, compiler tools,
runtime, package feed and project templates that match that command. You do
not need an Isoplane checkout, Rust, Mono, or a separately installed WASI SDK.

The current release is for Linux x64 with glibc; the validated baseline is
Ubuntu 24.04. Package restore needs internet access.

## Prerequisite: a .NET SDK

`dotnet tool install` needs a .NET 10 SDK. On Ubuntu 24.04, install the packaged
one, which also brings every native library the Isoplane toolchain needs:

```bash
sudo apt-get install -y dotnet-sdk-10.0
```

Setup reuses whatever `dotnet` you have and never changes your `DOTNET_ROOT`.
On another Linux distribution, install the .NET 10 SDK its packages provide,
or Microsoft's SDK with its own runtime in its own directory; do not mix the
two in one root (see [troubleshooting](troubleshooting.md)). Setup checks the
native libraries it needs before downloading anything and names what is
missing.

## Install the CLI and the SDK

```bash
dotnet tool install --global Isoplane.Cli
export PATH="$HOME/.dotnet/tools:$PATH"
isoplane setup
```

The tool installs into `~/.dotnet/tools`; putting that directory on `PATH` is
the standard .NET global-tool step, and setup adds it to new terminals for you.

Setup downloads the release the installed CLI was built from, verifies every
component against the release manifest, and installs it under `~/.isoplane`
(`ISOPLANE_HOME` overrides this). It registers the release's package feed with
NuGet and its project templates with `dotnet new`, records everything it did in
`~/.isoplane/installation.json`, and adds one line to `~/.profile` and, when
present, `~/.bashrc`, `~/.bash_profile` and `~/.zshrc` that sources
`~/.isoplane/env`. That file puts the installed SDK and `~/.dotnet/tools` first
on `PATH`. Before reporting success, setup runs the installed tools to prove
they work on your machine.

Open a new terminal, or load the environment in this one:

```bash
. "$HOME/.isoplane/env"
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
dotnet tool update --global Isoplane.Cli
isoplane setup
isoplane doctor
```

Setup installs the release matching the updated CLI, replaces the superseded
toolchain and template registration, and keeps earlier SDK packages so existing
applications still restore. Setup refuses a release the installed CLI was not
built from, naming the `dotnet tool update` command that installs the matching
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

This removes everything recorded in `~/.isoplane/installation.json`: the
toolchain, the package feed and its NuGet registration, the template
registration, the environment file and the startup-file lines. Resources that
existed before setup, your projects, and the CLI's own configuration and
sign-in state stay. Anything that could not be removed remains listed in the
manifest with the reason, and the command can be run again. Without a
manifest, or with a damaged one, the command explains and removes nothing.

The CLI itself remains so setup can be run again; remove it with
`dotnet tool uninstall --global Isoplane.Cli`.
