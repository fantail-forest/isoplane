---
title: Debug an application
---

Install the [Isoplane extension from the Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=Isoplane.isoplane)
and the matching Isoplane CLI and SDK to debug your application in VS Code.
You can also find the extension in VS Code's Extensions view by searching for
`@id:Isoplane.isoplane`. Choose **Install Pre-Release Version** to use the
preview extension.

## Launch or attach

With the extension installed, open your application
folder, choose **Debug local Isoplane**, set a breakpoint, and press **F5**. A
project created from a template has that configuration in `.vscode/launch.json`;
[launch.json for VS Code](reference/vscode-launch.md) lists its attributes.
C# Dev Kit's **Debug project associated with this file** button starts the same
session for an Isoplane project. The
adapter builds and starts the application in an **Isoplane** terminal. Send an HTTP
request to the URL printed in the terminal or Debug Console. Stopping the debugger
stops the application and its owned processes. No application-code call or
`IsoplaneDebug` project property is needed for this workflow. The default build
configuration is Debug; optimized Release builds can have fewer inspectable locals
and source locations.

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

In VS Code, add the Isoplane **Attach to local Isoplane** configuration and enter
the printed debugger port when prompted. This is a different port from the
application's HTTP listener. The extension starts `isoplane debug-adapter` for
you; that command speaks the Debug Adapter Protocol to the editor, not an
interactive debugger prompt. Set a breakpoint before sending
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

## Inspect values and control breakpoints

The session uses the SDK's shared debug runtime. Use the editor's stepping,
hover, Watch, Locals, and value-editing controls. Source and function breakpoints,
conditional breakpoints, hit counts, and logpoints are supported. A hit condition
can be a positive count such as `5`, a comparison such as `>= 5`, or `% 5` to stop
on every fifth hit. Logpoints write their message without stopping execution.

The call stack shows application, package-dependency, Isoplane SDK and .NET
frames, and reconstructed async callers. Frames and async callers from assemblies
without symbols, such as .NET and ASP.NET Core, appear by name without a source
location. The call stack hides the Isoplane runtime's internal frames below the
SDK, the .NET frames they call to start or resume the application, and package
methods marked as generated or non-user code.

## Edit code during a session

In VS Code, saving source automatically applies compiler-supported Hot Reload
edits to the current guest, while running or paused. No separate command is
required. **Isoplane: Apply Code Changes** is an optional save-all-and-apply action.
Running updates briefly suspend execution without introducing an editor stop; new calls
use the edited methods, and real breakpoint or exception stops remain in effect.
Compilation errors preserve execution state and appear in the **Isoplane Hot
Reload** output channel. Correct the error and save again. Edits reported as
requiring a restart cannot be applied to the current guest; restart the session
to use them throughout the application.
Source-only line changes also update paused locations and inline-breakpoint suggestions.
Hot Reload tracks edits to source documents in the current compiler baseline.
Adding source files or changing project references requires a rebuild; those
changes do not update the active guest through save-triggered Hot Reload.

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

## Watch values and async callers

Use the variable's data-breakpoint action in Locals or Watch to track an ordinary
local, managed object field, or array element. Data breakpoints stop when a value
changes. A local watch expires when its lexical
scope or method invocation ends, including exception unwinding; the editor marks
it unverified. Select the local again in a later invocation to watch its new
storage. Parameters and by-reference locals cannot be watched directly. Reads
and writes that leave the value unchanged do not stop execution.
Data breakpoint identities are tied to the guest's storage: select the values
again after a fresh guest or debug session starts.

Select an async caller in the call stack to inspect its saved locals, evaluate
watches, edit values, or set data breakpoints on saved locals, captured object
fields and array elements. `Task` and `ValueTask` callers remain visible across
suspension in normal and optimized builds, including pooled async method builders,
custom awaiters, and `Task.WhenAll`/`Task.WhenAny` chains.
Only values retained in the suspended state machine can be inspected; a local
the compiler has optimized away or not saved across the await is unavailable.

The **Uncaught exceptions** filter also stops on failures the SDK converts into
guest traps. Exception details retain the original throw stack. At this boundary,
methods may already have unwound; their original locals are no longer live.
Disabling the filter lets the failed request finish without this stop.

## Move execution

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

Moving execution does not undo writes, I/O, or other effects of statements already
executed. Restarting a frame, reverse stepping, and time-travel debugging are not
supported. Debugging is local and uses the guest's single execution thread; full
Visual Studio and Rider IDE integration has not been qualified.

Use ordinary .NET unit tests for isolated application logic and
[sandbox integration tests](test-an-application.md) to exercise platform behavior.
