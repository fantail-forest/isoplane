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
adapter builds and starts the application in an **Isoplane** terminal. Send an HTTP
request to the URL printed in the terminal or Debug Console. Stopping the debugger stops the application and its owned
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

If symbols record a different source checkout, set `sourceFileMap` in the launch
or attach configuration, for example `"sourceFileMap": { "/build/app": "${workspaceFolder}" }`.
Stack frames and execution-move targets use the mapped local files; source
breakpoints also verify the file checksum when the build path differs.

In VS Code, choose **Isoplane: Disconnect and Keep Running** from the debug toolbar
or command palette. This prepares the ordinary runtime before disconnecting.
For a terminal-owned session, detaching resumes the current request and leaves
the application running. Subsequent requests
and source reloads use the ordinary runtime. Stop it with Ctrl+C in its terminal.
If that runtime cannot be prepared, detach is rejected and the paused debug session
remains available. IDE launches use the integrated terminal when supported. Setting
`"console": "internalConsole"` keeps output in the Debug Console but requires
termination on disconnect.

The session uses the SDK's shared debug runtime. The debugger supports stepping,
watches, editing live locals, exception stops, and data breakpoints on ordinary
locals, managed fields and array elements. Source edits rebuild the package while keeping the
debugger attached. A paused request keeps its current code and locals; continue
it to completion before the replacement takes effect. The next request runs the
rebuilt code with source breakpoints rebound. A failed build leaves the last
working application available. Use `--no-watch` (or `"watch": false` in an IDE
launch configuration) to disable automatic rebuilds. This rebuilds the application;
it does not apply in-place Hot Reload to the active request.

Data breakpoints stop when a value changes. A local watch expires when its lexical
scope or method invocation ends, including exception unwinding; the editor marks
it unverified. Select the local again in a later invocation to watch its new
storage. Parameters and by-reference locals cannot be watched directly. Reads
and writes that leave the value unchanged do not stop execution.

Select an async caller in the call stack to inspect its saved locals, evaluate
watches, edit values, or set data breakpoints on its captured fields and array
elements. Task-based callers remain visible across suspension, including custom
awaiters and `Task.WhenAll`/`Task.WhenAny` chains.

The **Uncaught exceptions** filter also stops on failures the SDK converts into
guest traps. Exception details retain the original throw stack. At this boundary,
methods may already have unwound; their original locals are no longer live.
Disabling the filter lets the failed request finish without this stop.

In the current physical frame, **Jump to Cursor** can move execution to another
statement in the same method without executing the intervening statements or
resetting locals. The debugger offers only locations with an empty evaluation
stack. Constructors, async and iterator methods, synchronized methods, and
methods containing exception handlers do not offer jump targets.

Use ordinary .NET unit tests for isolated application logic and
[sandbox integration tests](test-an-application.md) to exercise platform behavior.
