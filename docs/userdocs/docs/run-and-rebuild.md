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
cover your application and project references by default. In your application's
own C# code they appear in the editor on the call itself. Where a wait has an
asynchronous counterpart, a quick fix replaces it: `Thread.Sleep` with
`await Task.Delay`, `task.Wait()` with `await task`, `Task.WaitAll` with
`await Task.WhenAll`, and `SemaphoreSlim.Wait` with `await WaitAsync`. Project
references are reported from their compiled assemblies when the application
builds. Packaged dependencies
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

## Rebuild on every save

`isoplane dev` runs the application and watches its source:

```bash
isoplane dev
```

After an edit it rebuilds the application while the running one keeps serving. A
request in progress finishes on the code it started with, and the next request
runs the rebuilt code. If the build fails, the terminal says why and the last
working application stays available. Press Ctrl+C to end the session.

`isoplane dev` takes the project's directory with `--project` and its
configuration with `--configuration`, which is Debug when omitted. `--port`,
`--launch-profile` and `--no-launch-profile` choose what `dotnet run` takes from
the [launch profile](local-settings.md#use-the-launch-profile), and `--no-watch`
runs without watching.

The ordinary .NET watcher works too:

```bash
dotnet watch --no-hot-reload run
```

On an edit it stops the application, rebuilds, and starts it again, so the
application is unavailable in between. If compilation fails, fix the source; the
watcher retries after the next edit. `--no-hot-reload` requests that restart
directly. Isoplane applications do not support hot reload; without the flag the
.NET watcher reports that and falls back to restarting.

## Debugging

Debugging in VS Code needs the Isoplane debugger extension. It is not on the
Visual Studio Marketplace: it is a `.vsix` file, installed with VS Code's
**Extensions: Install from VSIX** command.

With the extension installed, open your application
folder, choose **Debug local Isoplane**, set a breakpoint, and press **F5**. A
project created from a template has that configuration in `.vscode/launch.json`.
C# Dev Kit's **Debug project associated with this file** button starts the same
session for an Isoplane project. The
adapter builds and starts the application in an **Isoplane** terminal. Send an HTTP
request to the URL printed in the terminal or Debug Console. Stopping the debugger stops the application and its owned
processes.

A breakpoint is shown as verified when a statement of the built application
starts on its line. That is checked when the session starts, when you add or move
a breakpoint, and again after a source edit rebuilds the application; it does not
wait for a request to reach the code. A breakpoint on a line with no statement
stays unverified and says so.

A line can hold more than one statement, such as a lambda inside a call. To stop
inside the lambda, put the caret in it and choose **Add Inline Breakpoint**
(Shift+F9 in VS Code). The breakpoint moves to the start of the statement the
caret is in. Once a line has a breakpoint, the editor also marks the other
statements on it that can take one.

The application runs inside the sandbox, so the ordinary C# debugger cannot reach
it: attached to the `isoplane` tool, it leaves breakpoints in the application
unverified. Running without debugging, from C# Dev Kit's **Run project** button or
`dotnet run`, starts the application without a debugger.

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
locals, managed fields and array elements. In VS Code, saving source applies
compiler-supported Hot Reload edits to the current guest, while running or paused.
**Isoplane: Apply Code Changes** saves and applies pending edits as well. Running
updates briefly suspend execution without introducing an editor stop; new calls
use the edited methods, and real breakpoint or exception stops remain in effect.
Compilation errors preserve execution state and appear in the **Isoplane Hot
Reload** output channel.
Source-only line changes also update paused locations and inline-breakpoint suggestions.

At source-breakpoint, line-step, and `Debugger.Break()` stops, the active frame adopts
compatible edits, so **Continue** and **Step** execute the updated method. Callers
paused in method or constructor calls adopt compatible edits when those calls return,
without repeating the calls or losing values already computed in the statement.
Other active invocations retain their original instructions until a supported
**Jump to Cursor** adopts the edited method. Existing locals are preserved. Remapping
can add locals and compiler-generated temporaries while preserving existing local
types and storage addresses. Edits that change existing local storage or active
exception-handler state cannot be adopted by that invocation.

Source watching also rebuilds the application package for subsequent requests,
with source breakpoints rebound. A failed build leaves the last working package
available. Use `--no-watch` (or `"watch": false` in an IDE launch configuration)
to disable those rebuilds. Hot Reload updates the active guest; without a rebuild,
the next request still starts from the original application package.

Data breakpoints stop when a value changes. A local watch expires when its lexical
scope or method invocation ends, including exception unwinding; the editor marks
it unverified. Select the local again in a later invocation to watch its new
storage. Parameters and by-reference locals cannot be watched directly. Reads
and writes that leave the value unchanged do not stop execution.

Select an async caller in the call stack to inspect its saved locals, evaluate
watches, edit values, or set data breakpoints on saved locals, captured object
fields and array elements. `Task` and `ValueTask` callers remain visible across
suspension in normal and optimized builds, including pooled async method builders,
custom awaiters, and `Task.WhenAll`/`Task.WhenAny` chains.

The **Uncaught exceptions** filter also stops on failures the SDK converts into
guest traps. Exception details retain the original throw stack. At this boundary,
methods may already have unwound; their original locals are no longer live.
Disabling the filter lets the failed request finish without this stop.

In the current physical frame, **Jump to Cursor** can move execution to another
statement in the same method without executing the intervening statements or
resetting locals. The debugger offers only locations with an empty evaluation
stack. Jumps can enter or leave `try` regions, or stay within an active `catch`
or `finally` handler. Jumping out of a `try` skips its cleanup; exiting it through
ordinary execution still runs `finally`. Jumps within handlers preserve the
active exception and pending `finally` continuation. They cannot enter or leave
handlers, or move execution inside exception filters. Active C# async frames support jumps
across `await` statements within those handler boundaries, including jumping
backward to await again. Suspended async callers do not have a live instruction
pointer to move. Constructor bodies and synchronized methods support statement
jumps; a synchronized method retains its monitor until ordinary method exit.
Active synchronous and asynchronous iterator bodies support jumps across `yield`
statements. A jump preserves the current iterator values; ordinary enumeration
and disposal still follow the resulting control flow.

Use ordinary .NET unit tests for isolated application logic and
[sandbox integration tests](test-an-application.md) to exercise platform behavior.
