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
dotnet new isoplane-web --name MyApp
```

Setup installs the Isoplane workload into your existing .NET SDK and registers
the release's NuGet feed. It requests administrator access if the SDK directory
requires it. Release packages and ownership records live in `~/.isoplane`;
setup preserves your shell configuration and SDK selection. Add
`~/.dotnet/tools` to your shell startup configuration for future terminals.
Without `--source`, setup downloads the release matching the installed CLI.
`isoplane doctor` diagnoses installation problems. `isoplane uninstall-sdk`
removes owned SDK resources and keeps the CLI.

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

Third-party attribution and license texts are in `notices/` inside the Isoplane
toolchain workload pack, with upstream notices retained in the runtime and build packs.
