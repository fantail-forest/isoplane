---
title: Troubleshoot local development
---

Start with `isoplane doctor` in the terminal where the application fails. Keep
its output, the selected configuration, and the first build or runtime error
when reporting a problem.

## `dotnet tool install` cannot find `DotnetToolSettings.xml`

The package is intact; the SDK cannot map this machine's runtime identifier
to the package's `any` tools folder. This happens when a Microsoft-built SDK
runs on a distribution-built runtime in the same root, such as a 10.0.4xx SDK
placed into Ubuntu's `/usr/lib/dotnet`, where the runtime reports
`ubuntu.24.04-x64`, a runtime identifier Microsoft's SDK does not know. Keep
one vendor per root: install the SDK from your distribution's packages, or
install Microsoft's SDK together with its runtime in its own directory
(for example `~/.dotnet` with `DOTNET_ROOT` set), and remove the other from
that root. `dotnet --info` shows which SDK and runtime identifier are active.

## Command or SDK not found

Check that `~/.dotnet/tools` is on `PATH` so your shell can find `isoplane`.
`dotnet --info` shows your selected SDK. Isoplane uses that SDK's workload
installation; after switching SDK feature bands, run `isoplane setup` again.
Projects do not need a machine-specific SDK path.

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
  --configuration IsoplaneInterpretedDebug --json \
  >isoplane-session.jsonl 2>isoplane-debug.jsonl
```

The session file contains machine-readable ready and reload events. The debug
file contains the runtime diagnostics to attach to the report.

## A database certificate is rejected

Keep hostname and chain validation enabled. Use the database provider's TLS
hostname, and check its certificate chain and trusted CA configuration. Do not
switch off TLS or accept every certificate to make the test pass.
