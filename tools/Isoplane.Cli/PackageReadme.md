# Isoplane CLI

The `isoplane` tool runs .NET applications locally in Isoplane's WebAssembly
sandbox.

```bash
dotnet tool install --global Isoplane.Cli --prerelease
isoplane setup
dotnet new isoplane-web --name MyApi
cd MyApi
dotnet run
```

`isoplane setup` checks prerequisites and installs the project templates.
Building and running an application needs the .NET 10 SDK and this tool; it needs
no workload, native compiler or Docker. Windows also needs the Microsoft Visual
C++ x64 Redistributable.

| Command | What it does |
| --- | --- |
| `isoplane dev` | Runs the application in the current directory and rebuilds it on every save |
| `isoplane dev --debug` | Runs it for a debugger to attach |
| `isoplane doctor` | Checks this machine's .NET SDK, native library and templates |
| `isoplane setup` | Installs the templates that match this tool; `isoplane uninstall-sdk` removes them |

`dotnet run` in an Isoplane project starts the same local session.
`isoplane --help` lists every command.

Keep this tool, the project SDK packages and the templates at the same version.

`isoplane telemetry status` shows whether this tool collects anonymous usage
data, and `isoplane telemetry disable` turns that off.
[Telemetry](https://isoplane.dev/docs/reference/telemetry) lists what is collected.

Learn more at [isoplane.dev](https://isoplane.dev/).
