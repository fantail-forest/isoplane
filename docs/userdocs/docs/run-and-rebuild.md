---
title: Build and run
---

import {OnlyOn, PlatformToggle} from '@site/src/components/Platform';

<PlatformToggle />

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

Set `ISOPLANE_CACHE_DIR` to an absolute path to keep the cache there instead,
in a `runtimes` folder; when `ISOPLANE_HOME` is set and `ISOPLANE_CACHE_DIR` is
not, it is `ISOPLANE_HOME/cache/runtimes`. One runtime takes about 150 MB, and
the debug runtime another 80 MB once you debug. The cache keeps at most 1 GB,
removing the runtimes used least recently.

```bash
isoplane cache info
isoplane cache clean
```

`isoplane cache info` prints the cache's location, its size and when each
compiled runtime was last used. `isoplane cache clean` removes every compiled
runtime, including one a running session is using: that session keeps running,
and the next session to start compiles the runtime again. On Windows a runtime
that a running session has open cannot be removed; the command says so, and
removes it when you run it again after stopping the session.

`isoplane prepare-runtime --runtime DIRECTORY` compiles a runtime into the
cache before any session needs it, for example while building a CI image.
`DIRECTORY` is the `tools/runtime` folder of an `Isoplane.Sdk` package.

What a session prepares from your project alone goes under the project's
MSBuild intermediate output directory (`obj/` by default), and its application
package and session state under the project's `.isoplane/` directory. To keep
all of that outside the project, for example when the project is on a network
drive, set `ISOPLANE_TARGET_DIR` to an absolute path outside it. Each project
then gets a folder of its own under that directory.

## Build the same package on any machine

An application's package records when each of its files was last written. An
ASP.NET Core application's package records it a second time for the files in
`wwwroot`, in the manifest ASP.NET Core serves them from. Two builds of the same
source therefore differ when their files were written at different times, as
they are after a fresh checkout, and they report different versions.

To make them identical, set
[`SOURCE_DATE_EPOCH`](https://reproducible-builds.org/docs/source-date-epoch/)
to a number of seconds since 1970, by convention the time of the commit being
built:

<OnlyOn platforms="mac linux">

```bash
SOURCE_DATE_EPOCH=$(git log -1 --format=%ct) dotnet run
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
$env:SOURCE_DATE_EPOCH = git log -1 --format=%ct
try { dotnet run }
finally { Remove-Item Env:SOURCE_DATE_EPOCH }
```

</OnlyOn>

Every file in the package then carries that time. The application sees it as
each file's modification time, and ASP.NET Core sends it as the `Last-Modified`
of each static file. A value that is not such a number stops the build with a
message that names the variable.

A package never records the user or the file permissions of the machine that
built it.

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

Source watching follows the .NET SDK's evaluated watch list, including referenced
projects and linked source files, and each project's `Directory.Build.props` and
`Directory.Build.targets`. Add other build inputs with MSBuild `Watch` items in
the project. Restart the session after changing launch profiles, User Secrets or
environment variables.

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
