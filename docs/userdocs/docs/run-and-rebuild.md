---
title: Build and run
---

Use the configuration selected in your project file. The examples below use the
configurations supplied by the template. See [execution modes](execution-modes.md)
for the differences between them.

## Build and run a configuration

```bash
dotnet build -c IsoplaneInterpretedDebug
dotnet run --no-build -c IsoplaneInterpretedDebug
```

Stop the application with Ctrl+C before starting another session for that
project. To check ahead-of-time compilation:

```bash
dotnet build -c IsoplaneNativeAotRelease
dotnet run --no-build -c IsoplaneNativeAotRelease
```

A successful build publishes and validates WebAssembly, pre-initializes the
application with its local settings, and compiles native code for local execution.
Preparation failures are build failures. `--no-build` starts the prepared application;
omit it when you want `dotnet run` to check for source changes and build first.
Changed module bytes, settings, or runtime invalidate the corresponding cached
preparation.

Test the configuration you intend to use, even if you normally iterate with
the interpreted configuration. See [execution modes](execution-modes.md) when a
NativeAOT build exposes different compatibility requirements.

## Watch for source changes

```bash
dotnet watch --no-hot-reload run -c IsoplaneInterpretedDebug
```

This uses the ordinary .NET watch workflow: on an edit it stops the application,
rebuilds, and starts it again. If compilation fails, fix the source; the watcher
retries after the next edit. The application is unavailable between stopping and
the successful restart. Press Ctrl+C to end the session.

`--no-hot-reload` requests a restart instead of applying code changes to the
running instance. Isoplane guests do not support hot reload; without the flag, the
.NET watcher reports that limitation and falls back to restarting. Use the same
configuration you would pass to `dotnet run`.

## Debugging

Breakpoint debugging is currently unavailable. Use ordinary .NET unit tests
for isolated application logic and [sandbox integration tests](test-an-application.md)
to exercise platform behavior in both modes.
