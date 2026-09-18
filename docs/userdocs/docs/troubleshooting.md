---
title: Troubleshoot local development
---

Start with `isoplane doctor` in the terminal where the application fails. Keep
its output, the selected configuration, and the first build or runtime error
when reporting a problem.

## Command or SDK not found

Check that `DOTNET_ROOT` points to `$ISOPLANE_HOME/dotnet` and that this directory
and `$HOME/.dotnet/tools` are on `PATH`. New terminals need the environment
settings from [installation](install.md). A system SDK can select different
workloads from the toolchain installed by setup.

## Missing or damaged toolchain

Run `isoplane setup --source` with the complete release directory, then run
`isoplane doctor` again. Setup refuses a release the installed CLI was not built
from and prints the `dotnet tool update` command that installs the matching CLI. Do not combine pack files from different
releases or manually repair hashes. If doctor reports a missing system library,
install the package for your distribution; the bundle does not replace glibc or
the operating system's native dependencies.

## Package restore cannot find the SDK

Check the application's `nuget.config`. It needs the local release `packages`
source as well as the feeds supplied by the template. Confirm that the directory
still exists and the project references the package version in that release.

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
