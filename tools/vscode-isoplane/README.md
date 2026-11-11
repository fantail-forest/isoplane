# Isoplane

**Ship your .NET API. Skip the infrastructure.**

Build and debug Isoplane applications directly in VS Code: launch your API, set
breakpoints, inspect variables, and rebuild source edits for the next request.

This extension supports local development. It requires VS Code
1.95 or later, the .NET 10 SDK, and the Isoplane CLI and SDK installed separately.
The CLI must be available on VS Code's PATH, or selected with `cliPath`.
See [isoplane.dev](https://isoplane.dev) for the platform.

## Debug your application

Install the Isoplane CLI and SDK, open your application folder in VS Code, and
choose **Run and Debug → Isoplane → Debug local Isoplane**. Set a breakpoint and
press **F5**. The adapter builds and starts the application; the Debug Console
prints its local HTTP URL. Send a request to that URL to reach the breakpoint.

```json
{
  "type": "isoplane",
  "request": "launch",
  "name": "Debug local Isoplane",
  "project": "${workspaceFolder}",
  "configuration": "Debug",
  "port": 0
}
```

`port` selects the HTTP listener port; zero chooses an available port.

C# Dev Kit's **Debug project associated with this file** button, and a `dotnet`
or `coreclr` launch configuration that starts an Isoplane project, start this
same launch. The C# debugger would attach to the `isoplane` tool, which only
hosts the application, and leave the application's breakpoints unverified.
Running without debugging is left to the project's run command.

Keep the debugger attached for subsequent requests. Breakpoint configuration
is retained as each fresh guest starts. Saving source during an active request
automatically applies Hot Reload edits, while running or paused. No separate
command is required; **Isoplane: Apply Code Changes** is an optional
save-all-and-apply action.
Compiler diagnostics appear in **Isoplane Hot Reload**. Use **Jump to Cursor** to
repeat an edited statement in the current invocation while preserving its locals.
Supported active frames adopt edited statements automatically. Jump destinations
must preserve valid exception handling and async or iterator state.

Source edits also rebuild the application package for subsequent requests. A
failed rebuild preserves the last working application. Set `"watch": false` to
disable rebuilds; Hot Reload edits then affect only the active guest.

Stopping the debugger ends the local development session.

To attach to a session started in a terminal, run
`isoplane dev --debug --no-watch` and use its printed debugger port:

```json
{
  "type": "isoplane",
  "request": "attach",
  "name": "Attach to local Isoplane",
  "port": 12345
}
```

Set `cliPath` in that configuration if `isoplane` is not on VS Code's PATH.
It accepts an executable or a CLI `.dll` assembly; assemblies run through `dotnet`.

## License

This extension is supplied under the Apache License 2.0. See `LICENSE.txt`
in the extension package.
