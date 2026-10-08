# Isoplane ASP.NET adapter

`Isoplane.AspNetCore` runs ASP.NET routing, middleware and request-scoped dependency
injection over Isoplane requests. It replaces the server transport, so the
application does not bind a listener.

Use the package's .NET SDK entry point so its compiler and target settings are
available to the first restore:

```xml
<Project Sdk="Isoplane.AspNetCore/VERSION">
  <PropertyGroup>
    <Nullable>enable</Nullable>
    <ImplicitUsings>enable</ImplicitUsings>
  </PropertyGroup>
</Project>
```

The application is its `Program.cs`:

```csharp
using Isoplane.AspNetCore;

var builder = IsoplaneHosting.CreateBuilder(args);
var app = builder.Build();

app.MapGet("/", () => "Hello from ASP.NET on Isoplane");

await app.RunAsync();
```

Register services and middleware as usual. `CreateBuilder` reads `appsettings.json`,
`appsettings.{Environment}.json` and the settings the platform supplies; it does
not read the machine's environment variables. End the entry point with
`await app.RunAsync()`: the synchronous `app.Run()` blocks, which a sandboxed
application cannot do, and the build rejects it.

Isoplane runs the entry point during pre-init, up to the await on `RunAsync`,
which starts the .NET host and its hosted services. Initialization hooks run
next, and the result is captured as the template every instance starts from.
Keep request-specific state out of the top level of `Program.cs`. Request scopes
are created and disposed by ASP.NET's normal request lifecycle. A
`BackgroundService` still running once the host has started is refused by name:
its work cannot continue beyond a completed dispatch. One that finishes during
startup is accepted, as is bounded asynchronous `IHostedService.StartAsync` work.

To warm a read endpoint through middleware, binding and serialization before capture:

```csharp
Isoplane.Initialization.Initializing += () => app.WarmupAsync("/todos");
```

The warmup consumes the response locally and fails for unsuccessful HTTP statuses.
Choose endpoints whose execution is safe during preparation. Their scoped services
are disposed normally before capture, and the started host is reused by real requests.

Responses preserve status, binary bodies and repeated headers. `OnStarting`
runs before headers become immutable. For requests that finish through the
adapter, `OnCompleted` runs before the request context is disposed. Disconnect
or eviction can end guest execution before callbacks run. `RequestAborted`
receives Isoplane's dispatch cancellation wake and application-triggered aborts.
Response writes and flushes can reach the client while the handler awaits I/O.
`CompleteAsync` closes application writes; the host sends EOF after the handler
and completion callbacks finish. A failure after headers terminates the body
with a transport error. Request bodies are asynchronous, non-seekable streams:
handlers can read and respond before the client finishes its upload. A file
response is read from the application package and written as the body, so static
files are served. Connection upgrades are unsupported.

Cancellation is checked before each upload read; it cannot interrupt a pending
host read or an individual response write awaiting acknowledgement. Disposing
an upload prevents new reads, while its host transport remains owned until the
dispatch settles. An early response releases unread content without background
draining. Streaming also does not yet impose a byte
cap on pending write payloads; that resource limit remains unresolved.

Build the application with the pinned .NET SDK. `isoplane dev` and `isoplane bundle`
restore the selected managed ASP.NET runtime and request-delegate generator from
NuGet and package application IL. They do not require a native guest compiler.

Set `RuntimeFrameworkVersion` in the application project to select the ASP.NET
patch version (for example, `10.0.12`). Without an explicit selection, it uses the latest
patch listed by the .NET SDK. Ordinary `PackageReference` versions remain owned by
the application. Deployment bundles carry those resolved implementations;
the platform supplies the shared WASM runtime.

To build local packages, pack `Isoplane.Sdk.csproj` and `Isoplane.AspNetCore.csproj` into
the same directory and configure that directory as a NuGet source. The installed
SDK tests exercise this path with an isolated consumer and package cache.
`guests/aspnet` is an engine fixture for scope capture and streaming, not a customer
project. Managed adapter tests run with
`dotnet test guest-sdks/dotnet/AspNetCore.Tests`.

## Start from a template

Pack `Templates/Isoplane.Templates.csproj` alongside the two SDK packages in your
local NuGet feed. Install that template package and generate an application:

```sh
dotnet new install /path/to/feed/Isoplane.Templates.VERSION.nupkg
dotnet new isoplane-web --name MyApp
cd MyApp
dotnet nuget add source /path/to/feed --name isoplane-local --configfile nuget.config
isoplane dev
```

The generated project includes an ASP.NET route, `appsettings.json` and a launch
profile. `isoplane-webapi` and `isoplane-handler` generate larger starting points
with tests. Install the [.NET CLI](../../../tools/Isoplane.Cli/README.md)
and put its tool directory on your PATH. `isoplane dev` builds an application
package and serves it on the SDK's shared Mono runtime; source edits trigger a
rebuild. Local state is private to the project directory.
