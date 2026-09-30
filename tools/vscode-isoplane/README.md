# Isoplane Debugger

Install the Isoplane CLI and SDK, then start an interpreted debug session from
the application directory:

```sh
dotnet run -c Debug -p:IsoplaneDebug=true
```

In VS Code, choose **Run and Debug → Isoplane** and enter the printed debugger port. Set a
breakpoint, then make an HTTP request to the local application.

Keep the debugger attached for subsequent requests. Breakpoint configuration
is retained as each fresh guest starts. Stopping the debugger ends the local
development session.

An existing launch configuration can specify the port directly:

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
