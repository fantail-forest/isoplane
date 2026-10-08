---
title: Troubleshoot local development
---

Start with `isoplane doctor` from the application directory in the terminal where
it fails. Doctor checks the selected .NET SDK, the native host library and its
prerequisites, the template registration and saved-session permissions, naming
each check as it starts. It repairs nothing and does not require login. Keep its output, the selected configuration, and the first build or runtime
error when reporting a problem.

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
`dotnet --info` shows your selected SDK. Projects restore their Isoplane SDK
from NuGet and do not need a machine-specific SDK path or custom workload.

## Missing or damaged CLI

Run `isoplane doctor` to check prerequisites and the bundled native library.
Reinstall the `Isoplane.Cli` .NET tool if its files are missing or damaged.
Run `isoplane setup` to restore template registration. If a system library is
missing, install the package named by setup or doctor.

## Package restore cannot find the SDK

Check `dotnet nuget list source`: nuget.org must be enabled for published releases.
A `nuget.config` with `<clear />` may hide user-level feeds. For local releases,
`isoplane setup --source RELEASE_DIRECTORY` registers the verified packages as
`isoplane` at `~/.isoplane/packages`. Check that the requested version is available
in the selected feed.

## An application build fails

Read the first compiler or platform-validation error. The sandbox does not
provide arbitrary operating-system capabilities. Changing a timeout or disabling
certificate validation is not a fix for an unsupported API.

An error whose code starts with `ISOPLANE` names an Isoplane requirement, at the
line that breaks it where there is one. Your editor shows the same errors as you
type and offers a quick fix for many of them; see
[handling requests](handle-requests-without-aspnet.md) and
[compatibility warnings](run-and-rebuild.md#control-compatibility-warnings).

Debug and Release run on the same runtime and differ only in compiler
optimization, so a failure in one is rarely absent from the other.

## Requests fail or the app exits

Keep the terminal open while running locally. Build and runtime diagnostics are
written there. In a test, include `app.DiagnosticOutput` in failure diagnostics.
Check that request work is awaited and that network calls use asynchronous APIs.
See [the application lifecycle](application-lifecycle.md) before relying on
background work or retained state.

## The application refuses to start

Read the message printed with the failure; it names what stopped preparation.
Three causes are particular to Isoplane. A `BackgroundService` that is still
running once the host has started is refused by name, because work cannot
continue between requests. An entry point that returns without awaiting
`RunAsync` cannot serve requests. An initialization callback that throws, such as
a warm-up request to a database that is not running, fails preparation.

## Breakpoints stay grey

A grey breakpoint in a running debug session has not been matched to code. If
every breakpoint is grey, the ordinary C# debugger is attached to the `isoplane`
tool instead of the Isoplane debugger to your application: start the session
with **Debug local Isoplane**, as in [debugging](run-and-rebuild.md#debugging).
If one is grey, no statement starts on its line, which its hover text says, or
its file is not part of the built application.

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
  --configuration Debug --json \
  >isoplane-session.jsonl 2>isoplane-debug.jsonl
```

The session file contains machine-readable ready and reload events. The debug
file contains the runtime diagnostics to attach to the report.

## A database certificate is rejected

Keep hostname and chain validation enabled. Use the database provider's TLS
hostname, and check its certificate chain and trusted CA configuration. Do not
switch off TLS or accept every certificate to make the test pass.
