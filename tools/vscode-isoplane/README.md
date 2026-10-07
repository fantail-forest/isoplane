# Isoplane Debugger

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
is retained as each fresh guest starts. Source edits rebuild automatically; a
paused request keeps its existing code and locals until it finishes. The next
request uses the rebuilt application. A failed rebuild preserves the last working
application. Set `"watch": false` to disable rebuilds. This is request-boundary
replacement, not in-place Hot Reload.

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

For development, run `npm ci` and `npm run package` in this directory. Install
the resulting VSIX with VS Code's **Extensions: Install from VSIX** command.
`ISOPLANE_TEST_CLI=/absolute/path/to/isoplane npm test` exercises registration
and the real CLI adapter inside VS Code. Linux CI needs a display, such as
`xvfb-run -a npm test`.

The full editor journey launches a prepared customer application, inspects hover
and watch expressions, edits a local, and debugs a second request. From the
repository root, run `task test:editor`. Task prepares the CLI, application and
editor dependencies; Linux uses `xvfb-run`. Set `ISOPLANE_TEST_CLI` to exercise
a different CLI executable or assembly. Use `task test:editor -- suite` for the
registration and adapter checks.
