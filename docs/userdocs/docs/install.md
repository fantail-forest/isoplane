---
title: Install the SDK
---

import {OnlyOn, PlatformToggle} from '@site/src/components/Platform';

<PlatformToggle />

Isoplane installs as a .NET tool. You install the .NET 10 SDK, then the
`isoplane` CLI, then run `isoplane setup` once. Applications fetch the Isoplane
SDK and runtime from NuGet when they build, so there is nothing else to install.

Isoplane runs on Linux x64, Windows x64 and macOS on Apple silicon. Installing
needs internet access.

## Install the .NET 10 SDK

Isoplane needs version 10 of the .NET SDK and accepts no other, older or newer.
If `dotnet --version` already prints a version starting with `10.0`, go on to
[install the CLI](#install-the-cli).

<OnlyOn platforms="linux">

Isoplane needs a distribution built on glibc 2.39 or newer. The releases below
qualify. Ubuntu 22.04, Debian 12 and RHEL 9 are too old, and distributions
built on musl, such as Alpine, are not supported.

**Ubuntu 24.04 or later**

```bash
sudo apt-get update && sudo apt-get install -y dotnet-sdk-10.0
```

**Debian 13.** Debian does not package .NET, so add Microsoft's repository
first:

```bash
wget https://packages.microsoft.com/config/debian/13/packages-microsoft-prod.deb
sudo dpkg -i packages-microsoft-prod.deb
sudo apt-get update && sudo apt-get install -y dotnet-sdk-10.0
```

**Fedora, and RHEL 10 with its rebuilds such as AlmaLinux**

```bash
sudo dnf install -y dotnet-sdk-10.0
```

**Arch Linux.** Arch packages the ASP.NET targeting pack separately, and an
ASP.NET application does not build without it:

```bash
sudo pacman -S dotnet-sdk-10.0 aspnet-targeting-pack-10.0
```

**openSUSE Leap 16.** openSUSE does not package .NET either, so add Microsoft's
repository first:

```bash
sudo rpm --import https://packages.microsoft.com/keys/microsoft-2025.asc
sudo zypper addrepo https://packages.microsoft.com/config/opensuse/16/prod.repo
sudo zypper install dotnet-sdk-10.0
```

**Another distribution.** Use its own .NET 10 package if it has one. Otherwise
install Microsoft's SDK
[with its script](https://learn.microsoft.com/dotnet/core/install/linux-scripted-manual),
which puts the SDK and its runtime in a directory of their own such as
`~/.dotnet`. Do not copy a Microsoft SDK into a directory that holds a
distribution's runtime, because that combination
[cannot install tools](troubleshooting.md#dotnet-tool-install-cannot-find-dotnettoolsettingsxml).
Isoplane also uses ICU, OpenSSL 3, zlib, libstdc++ and the system's CA
certificates. The packages above bring all of them; elsewhere, `isoplane setup`
names any that are missing.

</OnlyOn>

<OnlyOn platforms="win">

Install the SDK and the Visual C++ x64 Redistributable, which Isoplane's
native library needs:

```powershell
winget install Microsoft.DotNet.SDK.10
winget install Microsoft.VCRedist.2015+.x64
```

Without winget, run the x64 SDK installer from the
[.NET 10 download page](https://dotnet.microsoft.com/download/dotnet/10.0) and
the [redistributable's installer](https://aka.ms/vs/17/release/vc_redist.x64.exe).

</OnlyOn>

<OnlyOn platforms="mac">

Download the .NET 10 SDK installer for **Arm64** from the
[.NET 10 download page](https://dotnet.microsoft.com/download/dotnet/10.0) and
run it. Isoplane does not run on Intel Macs.

</OnlyOn>

## Install the CLI

```bash
dotnet tool install --global Isoplane.Cli --prerelease
```

Open a new terminal and check that the command is there:

```bash
isoplane --version
```

If your shell cannot find `isoplane`, the directory .NET installs tools into is
not on your `PATH`.

<OnlyOn platforms="linux">

That directory is `~/.dotnet/tools`. Some distributions' .NET packages do not
add it, Fedora's and Arch's among them. Add it yourself, then open a new
terminal:

```bash
echo 'export PATH="$PATH:$HOME/.dotnet/tools"' >> ~/.bashrc
```

</OnlyOn>

<OnlyOn platforms="mac">

That directory is `~/.dotnet/tools`. Add it, then open a new terminal:

```bash
echo 'export PATH="$PATH:$HOME/.dotnet/tools"' >> ~/.zshrc
```

</OnlyOn>

<OnlyOn platforms="win">

That directory is `%USERPROFILE%\.dotnet\tools`. Search the Start menu for
**Edit environment variables for your account**, add the directory to `Path`,
and open a new terminal.

</OnlyOn>

## Run setup

```bash
isoplane setup
```

Setup checks that your machine has what Isoplane needs and installs the
project templates, the `Isoplane.Templates` package that matches your CLI. It
ends by reporting that Isoplane is ready. It uses the `dotnet` you already
have and leaves your .NET installation and shell configuration alone.

<OnlyOn platforms="linux">

If a system library is missing, setup names it and stops before installing
anything. On Ubuntu 24.04 it also shows the one `sudo apt-get install` command
that provides what is missing and asks before running it; `isoplane setup --yes`
runs it without asking. On other distributions, install the packages for the
libraries it names and run `isoplane setup` again.

</OnlyOn>

<OnlyOn platforms="win">

If the Visual C++ Redistributable is missing, setup stops and says so. Install
it and run `isoplane setup` again.

</OnlyOn>

## Check the installation

```bash
isoplane doctor
dotnet new list isoplane
```

Doctor checks the .NET SDK, Isoplane's native library and the templates. It
reports problems and does not repair them. The template list shows
`isoplane-web`, `isoplane-webapi` and `isoplane-handler`.

Now [create your first application](quickstart.md#3-create-an-application).

## Update

Stop any running development sessions, then update the CLI and run setup again:

```bash
dotnet tool update --global Isoplane.Cli --prerelease
isoplane setup
isoplane doctor
```

Setup replaces the templates with the ones for the new CLI, so new projects
use the new SDK. Existing applications keep the SDK version they name. To move
one, change the version in its project file, in `<Project Sdk="Isoplane.AspNetCore/VERSION">`
or `Isoplane.Sdk/VERSION` for an application without ASP.NET, and in any
explicit Isoplane package references such as `Isoplane.Testing` in test
projects. Then rebuild and test it.

If your current CLI comes from an early preview that installed an Isoplane
workload or MSI, run `isoplane uninstall-sdk` with it before updating. The new
CLI does not know about those older installations.

## Repair

Run `isoplane setup` again to restore missing templates. If the CLI itself or
its native library is damaged, uninstall and reinstall the `Isoplane.Cli` tool.
Setup never rewrites the package references in your projects.

## Remove

```bash
isoplane uninstall-sdk
dotnet tool uninstall --global Isoplane.Cli
```

The first command removes what setup installed and the second removes the CLI.
Your projects, the CLI's configuration and your sign-in state stay.

<OnlyOn platforms="mac linux">

Setup records what it installed in `~/.isoplane/installation.json`.

</OnlyOn>

<OnlyOn platforms="win">

Setup records what it installed in `%USERPROFILE%\.isoplane\installation.json`.

</OnlyOn>

Setting `ISOPLANE_HOME` moves that directory. `isoplane uninstall-sdk` removes
only what the record lists as its own, and leaves anything that was there
before setup ran. Anything it could not remove stays in the record with the
reason, and you can run the command again. Without the record, or with a
damaged one, it explains and removes nothing.

## Install from a release directory

When you have been given a release as a directory instead of installing from
NuGet, point both commands at it:

```bash
dotnet tool install --global Isoplane.Cli --prerelease --add-source RELEASE_DIRECTORY/packages
isoplane setup --source RELEASE_DIRECTORY
```

`isoplane setup --source` also accepts the HTTP address of a release directory.
Setup verifies that the release matches your CLI and that its packages match
their checksums, copies them into the `packages` directory beside the
installation record, and registers that directory as a NuGet source. Running
the same `isoplane setup --source` again restores missing or damaged packages,
and `isoplane uninstall-sdk` removes the packages and the source. Builds still
fetch their other dependencies from NuGet, so this does not make them offline.
