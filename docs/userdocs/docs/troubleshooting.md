---
title: Troubleshoot local development
---

Start with `isoplane doctor` in the terminal where the application fails. Keep
its output, the selected configuration, and the first build or runtime error
when reporting a problem.

## Command or SDK not found

New terminals load `~/.isoplane/env`, which puts the installed SDK and
`~/.dotnet/tools` first on `PATH`, through the line setup added to your shell
startup files. In a terminal opened before setup, run `. "$HOME/.isoplane/env"`.
`command -v dotnet` should print `~/.isoplane/dotnet/dotnet`; a system SDK
first on `PATH` lacks the workloads the toolchain installs.

## Missing or damaged toolchain

Run `isoplane setup`, then `isoplane doctor` again. Setup reuses verified
components and downloads the rest; it refuses a release the installed CLI was
not built from and prints the `dotnet tool update` command that installs the
matching CLI. Do not combine files from different releases or repair hashes by
hand. If setup or doctor reports a missing system library, install the package
setup names; the toolchain does not replace glibc or the operating system's
native dependencies.

## Package restore cannot find the SDK

Setup registers the release feed as the NuGet package source `isoplane`, at
`~/.isoplane/packages`; `dotnet nuget list source` shows it. A `nuget.config`
with `<clear />` between the project and your home directory hides it. Confirm
the project references a package version that feed holds; `isoplane setup`
keeps the packages of every release it installed.

## An application build fails

Read the first compiler or platform-validation error. The sandbox does not
provide arbitrary operating-system capabilities. Changing a timeout or disabling
certificate validation is not a fix for an unsupported API.

Try the same application in both configurations when diagnosing a compilation
problem. A difference can reveal trimming or dynamic-code assumptions; it does
not make the failing configuration safe to deploy.

## Requests fail or the app exits

Keep the terminal open while running locally. Build and runtime diagnostics are
written there. In a test, include `app.DiagnosticOutput` in failure diagnostics.
Check that request work is awaited and that network calls use asynchronous APIs.
See [the application lifecycle](application-lifecycle.md) before relying on
background work or retained state.

## Application log messages

Use ASP.NET's `ILogger` through dependency injection or `app.Logger`, with the
`Microsoft.Extensions.Logging` extension methods. Emit messages while handling
a request. Do not log credentials or connection strings.

`Console.WriteLine` and `Console.Error.WriteLine` also write application logs.
Use `ILogger` when you need structured fields and log levels; the ASP.NET
integration registers its logging provider automatically.

Local development prints application messages in a compact level, message and
category format. Runtime diagnostics stay out of that view. To capture the full
structured runtime record for a bug report, run:

```bash
RUST_LOG=debug isoplane dev --project . \
  --configuration WispInterpretedDebug --json \
  >isoplane-session.jsonl 2>isoplane-debug.jsonl
```

The session file contains machine-readable ready and reload events. The debug
file contains the runtime diagnostics to attach to the report.

## A database certificate is rejected

Keep hostname and chain validation enabled. Use the database provider's TLS
hostname, and check its certificate chain and trusted CA configuration. Do not
switch off TLS or accept every certificate to make the test pass.
