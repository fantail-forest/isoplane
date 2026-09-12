---
title: Build and run
---

The project selects its execution mode through ordinary MSBuild configuration.
The template provides two configurations:

| Configuration | Execution |
| --- | --- |
| `WispInterpretedDebug` | Interpreted WebAssembly guest |
| `WispNativeAotRelease` | Ahead-of-time compiled WebAssembly guest |

`PublishAot` selects the mode in the project file. You do not need to configure
the underlying interpreter by name. The target remains `wasi-wasm` in both modes.

## Build and run a configuration

```bash
dotnet build -c WispInterpretedDebug
dotnet run -c WispInterpretedDebug
```

Stop the application with Ctrl+C before starting another session for that
project. To check ahead-of-time compilation:

```bash
dotnet build -c WispNativeAotRelease
dotnet run -c WispNativeAotRelease
```

A successful build includes WebAssembly publication and platform validation.
Compilation failures and unsupported imports are build failures. Fix them before
running the new application.

NativeAOT can expose trimming or dynamic-code assumptions that do not affect an
interpreted build. Test the configuration you intend to use, even if you normally
iterate with the interpreted configuration.

## Watch for source changes

```bash
dotnet watch --no-hot-reload run -c WispInterpretedDebug
```

This uses the ordinary .NET watch workflow: on an edit it stops the application,
rebuilds, and starts it again. If compilation fails, fix the source; the watcher
retries after the next edit. The application is unavailable between stopping and
the successful restart. Press Ctrl+C to end the session.

`--no-hot-reload` requests a restart instead of applying code changes to the
running instance. WISP guests do not support hot reload; without the flag, the
.NET watcher reports that limitation and falls back to restarting. Use the same
configuration you would pass to `dotnet run`.

## Debugging

The preview tools do not yet ship a supported debugger adapter. A configuration
named `Debug` does not establish breakpoint support. Use ordinary .NET unit tests
for isolated application logic and [sandbox integration tests](test-an-application.md)
to exercise platform behavior in both modes.
