# Isoplane Debugger

Install the Isoplane CLI and SDK, then start an interpreted debug session from
the application directory:

```sh
dotnet run -c IsoplaneInterpretedDebug -p:IsoplaneDebug=true
```

In VS Code, choose **Run and Debug → Isoplane** and enter the printed debugger port. Set a
breakpoint, then make an HTTP request to the local application.

Each local session serves one request. Start another CLI session to debug the
next request. Stopping the debugger ends its local guest session.

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

On Linux, NativeAOT uses a launch configuration and requires GDB 16 or later
with Python DAP support on VS Code's PATH. SDK setup does not install GDB.
The generated project provides `IsoplaneNativeAotDebug`:

```json
{
  "type": "isoplane",
  "request": "launch",
  "name": "Debug NativeAOT Isoplane",
  "project": "${workspaceFolder}",
  "configuration": "IsoplaneNativeAotDebug"
}
```

Set a C# source breakpoint, launch, and make a request to the HTTP address
printed in the Debug Console. NativeAOT supports source stepping, inspecting
and editing scalar locals, and native debugger watch expressions. It does not
provide the interpreted adapter's C# expression evaluator, managed field/array
data breakpoints, or managed async stepping. NativeAOT editor debugging is
not supported on macOS or Windows.

For development, run `npm ci` and `npm run package` in this directory. Install
the resulting VSIX with VS Code's **Extensions: Install from VSIX** command.
`ISOPLANE_TEST_CLI=/absolute/path/to/isoplane npm test` exercises registration
and the real CLI adapter inside VS Code. Linux CI needs a display, such as
`xvfb-run -a npm test`.
The installed NativeAOT editor journey runs through
`cargo test -p isoplane-native --test installed_dev vscode_launches_nativeaot`;
it builds its fixture and editor dependencies through the shared helpers and
requires GDB 16+, Node/npm and `xvfb-run`.
