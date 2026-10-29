# Isoplane

Isoplane builds and runs your .NET application inside a WebAssembly sandbox.
The current release is a preview SDK for local development and testing. The
hosted service is not available yet.

## Quickstart

With the .NET 10 SDK installed on Linux x64, Windows x64 or macOS ARM64, four
commands get an ASP.NET application running in the sandbox:

```bash
dotnet tool install --global Isoplane.Cli
isoplane setup
dotnet new isoplane-web --name MyApi && cd MyApi
dotnet run
```

`isoplane setup` checks your machine and installs the project templates. The
first `dotnet run` downloads the SDK packages and compiles the runtime, so it
takes a while; when the application is ready it prints its address. From there,
the [Quickstart](https://isoplane.dev/docs/quickstart) explains each step and
the tutorials build something real.

## Learn more

- **Documentation:** https://isoplane.dev/docs/
- **Quickstart:** https://isoplane.dev/docs/quickstart
- **Packages:** the `Isoplane.*` packages and the `Isoplane.Cli` tool on NuGet

## What this repository is for

This is the public home for Isoplane. The source code is not published here
yet, but we do plan to release significant portions of the project under a
permissable license. Until then, use this repository to:

- [Report a bug](../../issues/new?template=bug_report.yml) in the SDK, the CLI
  or the project templates.
- [Request a feature](../../issues/new?template=feature_request.yml).
- [Ask a question or share what you built](../../discussions) in Discussions.
- [Report a security issue](SECURITY.md) privately.

When filing a bug, include the output of `isoplane --version` and your
operating system. The issue form asks for both.
