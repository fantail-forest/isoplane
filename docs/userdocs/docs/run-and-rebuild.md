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

## Control compatibility warnings

Thread-start (`ISOPLANE1002`) and synchronous-wait (`ISOPLANE1003`) warnings
cover your application and project references by default. Packaged dependencies
are inspected for valid metadata but do not produce these advisory warnings.
Native-import declarations (`ISOPLANE1001`) are silent by default because a
declaration alone does not establish that an unsupported call will execute.

To inspect dependencies or enable native-declaration warnings, set the desired
properties in your project or `Directory.Build.props`:

```xml
<PropertyGroup>
  <IsoplaneAnalyzeDependencies>true</IsoplaneAnalyzeDependencies>
  <IsoplaneWarnOnNativeImports>true</IsoplaneWarnOnNativeImports>
</PropertyGroup>
```

Use `NoWarn` to suppress individual codes, `WarningsAsErrors` to promote them,
or `TreatWarningsAsErrors` with `WarningsNotAsErrors` for blanket promotion with
exceptions. For example, `<NoWarn>$(NoWarn);ISOPLANE1003</NoWarn>` suppresses
synchronous-wait warnings. These settings affect advisory diagnostics only;
assembly validation and sandbox restrictions still apply.

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
