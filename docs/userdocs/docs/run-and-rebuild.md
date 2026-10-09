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

The first session on a machine compiles the SDK's shared runtime to native code,
which takes several seconds. Every later session reuses it, in every project
that uses the same SDK version, and a change to your application or its settings
compiles nothing: only your application is prepared again.

The compiled runtime is kept in your user cache directory, outside your
projects:

| Platform | Location |
| -------- | -------- |
| Linux    | `$XDG_CACHE_HOME/isoplane/runtimes`, or `~/.cache/isoplane/runtimes` when `XDG_CACHE_HOME` is not set |
| macOS    | `~/Library/Caches/isoplane/runtimes` |
| Windows  | `%LOCALAPPDATA%\Isoplane\Cache\runtimes` |

When `ISOPLANE_HOME` is set, it is `ISOPLANE_HOME/cache/runtimes` instead. One
runtime takes about 230 MB. The cache keeps at most 1 GB, removing the runtimes
used least recently, and you can delete the directory at any time while no
session is running: the next session compiles the runtime again.

What a session prepares from your project alone goes under the project's
MSBuild intermediate output directory (`obj/` by default), and its application
package and session state under the project's `.isoplane/` directory.

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
properties in your project or `Directory.Build.props`.
[Project settings](reference/project-settings.md) defines each one:

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
directly. The .NET watcher cannot apply Hot Reload updates to Isoplane's guest
runtime; without the flag it reports that and falls back to restarting. Use the
[Isoplane debugger](debug-an-application.md) to apply edits to a running guest.

## Debugging

See [Debug an application](debug-an-application.md) for editor setup, breakpoints,
Hot Reload, inspecting values, and moving execution.
