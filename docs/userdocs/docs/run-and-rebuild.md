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

With the Isoplane debugger extension installed in VS Code, open your application
folder, choose **Debug local Isoplane**, set a breakpoint, and press **F5**. The
adapter builds and starts the application. Send an HTTP request to the URL printed
in the Debug Console. Stopping the debugger stops the application and its owned
processes.

To start a session in a terminal instead:

```bash
isoplane dev --debug
```

Attach through `isoplane debug-adapter` using the printed loopback address and
port. In VS Code with the Isoplane debugger extension installed, choose
**Run and Debug → Isoplane** and enter that port. Set a breakpoint before sending
an HTTP request to the application's local URL. Keep the debugger attached for
subsequent requests; source breakpoints are rebound for each fresh guest.

The session uses the SDK's shared debug runtime. The debugger supports stepping,
watches, editing live locals, exception stops, and data breakpoints on managed
fields and array elements. Source edits rebuild the package while keeping the
debugger attached. A paused request keeps its current code and locals; continue
it to completion before the replacement takes effect. The next request runs the
rebuilt code with source breakpoints rebound. A failed build leaves the last
working application available. Use `--no-watch` (or `"watch": false` in an IDE
launch configuration) to disable automatic rebuilds. This rebuilds the application;
it does not apply in-place Hot Reload to the active request.

In the current physical frame, **Jump to Cursor** can move execution to another
statement in the same method without executing the intervening statements or
resetting locals. The debugger offers only locations with an empty evaluation
stack. Constructors, async and iterator methods, synchronized methods, and
methods containing exception handlers do not offer jump targets.

Use ordinary .NET unit tests for isolated application logic and
[sandbox integration tests](test-an-application.md) to exercise platform behavior.
