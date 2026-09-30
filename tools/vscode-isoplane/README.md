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

Keep the debugger attached for subsequent requests. Breakpoint configuration
is retained as each fresh guest starts. Stopping the debugger ends the local
development session.

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

For development, run `npm ci` and `npm run package` in this directory. Install
the resulting VSIX with VS Code's **Extensions: Install from VSIX** command.
`ISOPLANE_TEST_CLI=/absolute/path/to/isoplane npm test` exercises registration
and the real CLI adapter inside VS Code. Linux CI needs a display, such as
`xvfb-run -a npm test`.
