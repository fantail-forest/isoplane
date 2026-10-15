# Isoplane developer preview

This release targets @RUNTIME_IDENTIFIER@. It contains `release.json` and a
NuGet package feed (`packages/`). `release.json` records the source revision and
package checksums; `package-version.txt` records the version.

@SDK_PREREQUISITES@

For a published release:

```sh
dotnet tool install --global Isoplane.Cli --version @PACKAGE_VERSION@
isoplane setup
dotnet new isoplane-web --name MyApp
```

Add `~/.dotnet/tools` (Windows: `%USERPROFILE%\.dotnet\tools`) to PATH if needed.
The CLI includes its native host library. Setup checks prerequisites and installs
the matching project templates through NuGet. Projects restore their SDK and
shared WASM runtime through NuGet; no Isoplane workload or MSI is installed.
Windows also requires Microsoft's Visual C++ x64 Redistributable.

For this local release directory, add `--add-source "$PWD/packages"` to the
CLI installation command and use `isoplane setup --source "$PWD"`. That verifies
the release identity and package checksums, copies packages into `~/.isoplane/packages`,
and registers that local feed. Dependencies may still require network access.

`isoplane doctor` checks prerequisites, the native library and template registration.
`isoplane uninstall-sdk` removes setup-owned templates and optional local feed
resources; remove the CLI separately with `dotnet tool uninstall --global Isoplane.Cli`.

Update the CLI and rerun setup for a new release. Existing projects keep their
pinned SDK version; update `<Project Sdk="Isoplane.AspNetCore/VERSION">` and any
Isoplane package references when upgrading an application. `dotnet run` invokes
`isoplane dev` through the project's SDK targets.

Third-party attribution and license texts are in `notices/` in the CLI and native
NuGet packages.
