---
title: Build and run
---

Build managed application code with the ordinary .NET configurations:

```bash
dotnet build
dotnet run
```

`dotnet run` builds an application package, prepares it on the SDK's shared Mono
runtime with local settings, and starts the local listener. Stop the application
with Ctrl+C before starting another session for that project. Use `-c Release`
when you want the Release configuration.

The local host caches preparation and native runtime compilation. Changes to the
application package, settings or runtime invalidate the corresponding cached
state. See [application packages](execution-modes.md) for the execution model.

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
dotnet watch --no-hot-reload run
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

Start a local debug session with:

```bash
isoplane dev --debug --no-watch
```

Connect an editor through `isoplane debug-adapter`. The session uses the SDK's
shared debug runtime and serves one request. Use ordinary .NET unit tests for
isolated application logic and [sandbox integration tests](test-an-application.md)
to exercise platform behavior.
