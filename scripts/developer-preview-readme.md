# Isoplane Linux developer preview

This release targets Linux x64 on Ubuntu 24.04. It contains `release.json`,
the package feed (`packages/`), the toolchain archive (`toolchain.tgz`) and a
sample. Every Isoplane package carries the version in `package-version.txt`;
`release.json` records the source revision and the checksum of each component.

Install the bootstrap .NET SDK, which brings every native library the toolchain needs:

```sh
sudo apt-get install -y dotnet-sdk-10.0
```

Then install the CLI as a global .NET tool and let it install the rest:

```sh
dotnet tool install --global Isoplane.Cli --version @PACKAGE_VERSION@ --add-source "$PWD/packages"
export PATH="$HOME/.dotnet/tools:$PATH"
isoplane setup --source "$PWD"
. "$HOME/.isoplane/env"
dotnet new isoplane-web --name MyApp
```

Setup installs the SDK, compiler tools, runtime, package feed and project
templates under `~/.isoplane`, registers the feed and templates with .NET,
records everything in `~/.isoplane/installation.json`, and adds one line to your
shell startup files so new terminals find `dotnet` and `isoplane`. Without
`--source`, setup downloads the release matching the installed CLI from the
release origin. Rerunning setup verifies and repairs the installation; `isoplane
doctor` only diagnoses. `isoplane uninstall-sdk` removes everything setup
recorded and keeps the CLI.

For a new release, run `dotnet tool update --global Isoplane.Cli --version VERSION
--add-source RELEASE/packages` and then `isoplane setup --source RELEASE`. Setup
refuses a release the installed CLI was not built from and names the update
command. Existing projects keep their pinned SDK version; update the version in
`<Project Sdk="Isoplane.AspNetCore/VERSION">` and any Isoplane package references
to `package-version.txt` when upgrading an application.

Package restore can also contact nuget.org and the .NET experimental compiler
feed; fully offline builds are not promised. Follow the customer documentation
for `dotnet run`, `dotnet watch`, testing and database configuration. Debugger
support is not established by successful installation.

Third-party attribution and license texts are in `notices/` inside the toolchain
archive, with additional upstream notices retained in the .NET SDK and runtime packs.
