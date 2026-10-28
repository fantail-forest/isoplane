---
title: Troubleshoot local development
---

import {OnlyOn, PlatformToggle} from '@site/src/components/Platform';

<PlatformToggle />

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

Check that the [.NET tools directory](install.md#install-the-cli) is on `PATH`
so your shell can find `isoplane`.
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

An error or warning whose code starts with `ISOPLANE` comes from Isoplane's
analyzers, reported at the line concerned where there is one. [Error codes](../reference/error-codes.md) says what
each one means and what to do about it. Your editor shows the same errors as you
type and offers a quick fix for many of them; see
[handling requests](handle-requests-without-aspnet.md) and
[compatibility warnings](run-and-rebuild.md#control-compatibility-warnings).

Debug and Release builds run on the same runtime and differ only in compiler
optimization, so a failure in one is rarely absent from the other. A debugging
session runs that runtime with its debugger agent.

## Requests fail or the app exits

Keep the terminal open while running locally. Build and runtime diagnostics are
written there. In a test, include `app.DiagnosticOutput` in failure diagnostics.
Check that request work is awaited and that network calls use asynchronous APIs.
See [the application lifecycle](../concepts/application-lifecycle.md) before relying on
background work or retained state, and
[how .NET differs on Isoplane](../concepts/dotnet-runtime-differences.md) for the calls that
fail in the sandbox.

`An error occurred while reading the key ring` in the log, followed by a
`CryptographicException` when data is protected, comes from a key store that
reads synchronously, such as `PersistKeysToDbContext`. Use one of the stores
that [keep the keys across builds](keep-data-protection-keys.md) in the sandbox.

## The application refuses to start

Read the message printed with the failure; it names what stopped preparation.
Two causes are particular to Isoplane. An initialization callback that throws, such as
a warm-up request to a database that is not running, fails preparation. So does
initialization that leaves a connection or stream open: the message names the
kind, for example `initialization retains a TCP socket`, and the fix is to
dispose it before the callback returns. See
[initialization](../concepts/application-lifecycle.md#initialization).

## Breakpoints stay grey

A grey breakpoint in a running debug session has not been matched to code. Read
its hover message first. If every breakpoint is grey, check that the session uses
the Isoplane debugger: the ordinary C# debugger attached to the `isoplane` tool
cannot reach the guest application. Start with **Debug local Isoplane**, as in
[debugging](debug-an-application.md).

Other causes include a line with no executable statement, a file outside the
built application, missing symbols, or source that does not match the built
package. A failed rebuild leaves the last working package running. If the package
was built in another checkout, use `sourceFileMap` to map its recorded paths to
your local source; a mapping does not bypass source-checksum validation.

## Saved edits do not affect the current request

Hot Reload on save requires an Isoplane source-debugging session and an active
guest request. Check **Isoplane Hot Reload** for compiler diagnostics and the
Debug Console for setup errors. Save again after correcting a rejected edit.
Some edits require restarting the session.

Source watching rebuilds the package for subsequent requests separately. With
`--no-watch` or `"watch": false`, Hot Reload changes the active guest but the next
request still uses the original package. Outside a debugging session, saving
through `isoplane dev` affects subsequent requests, not the request in progress.
See [editing during a session](debug-an-application.md#edit-code-during-a-session) for
active-frame behavior and the limits of **Jump to Cursor**.

## Application log messages

Use ASP.NET's `ILogger` through dependency injection or `app.Logger`, with the
`Microsoft.Extensions.Logging` extension methods. Emit messages while handling
a request. Do not log credentials or connection strings.

`Console.WriteLine` and `Console.Error.WriteLine` also write application logs.
Use `ILogger` when you need structured fields and log levels; the ASP.NET
integration registers its logging provider automatically.
An application that adds the console logger, with `AddConsole` or one of its
formatter methods such as `AddJsonConsole`, logs each message as that formatter
writes it instead.

Local development prints application messages in a compact level, message and
category format. Runtime diagnostics stay out of that view. To capture the full
structured runtime record for a bug report, run:

<OnlyOn platforms="mac linux">

```bash
RUST_LOG=debug isoplane dev --project . \
  --configuration Debug --json \
  >isoplane-session.jsonl 2>isoplane-debug.jsonl
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
$env:RUST_LOG = "debug"
try {
    isoplane dev --project . --configuration Debug --json >isoplane-session.jsonl 2>isoplane-debug.jsonl
}
finally { Remove-Item Env:RUST_LOG }
```

</OnlyOn>

The session file contains machine-readable ready and reload events. The debug
file contains the runtime diagnostics to attach to the report.

## A database certificate is rejected

Keep hostname and chain validation enabled. Use the database provider's TLS
hostname, and check its certificate chain and trusted CA configuration. Do not
switch off TLS or accept every certificate to make the test pass.
