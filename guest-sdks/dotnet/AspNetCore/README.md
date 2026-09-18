# Isoplane ASP.NET adapter

`Isoplane.AspNetCore` runs ASP.NET routing, middleware and request-scoped dependency
injection over Isoplane requests. It replaces the server transport and
generates the platform exports. The guest does not bind a listener.

Use the package's .NET SDK entry point so its compiler and target settings are
available to the first restore:

```xml
<Project Sdk="Isoplane.AspNetCore/VERSION">
  <PropertyGroup>
    <IsoplaneApplicationFactory>MyApp.Application.Create</IsoplaneApplicationFactory>
    <ImplicitUsings>enable</ImplicitUsings>
  </PropertyGroup>
</Project>
```

The named static method returns a built `WebApplication`:

```csharp
using Microsoft.AspNetCore.Builder;
using Isoplane.AspNetCore;

namespace MyApp;

public static class Application
{
    public static WebApplication Create()
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        var app = builder.Build();
        app.MapGet("/", () => "Hello from ASP.NET on Isoplane");
        return app;
    }
}
```

Register services and middleware as usual. `CreateBuilder` uses ASP.NET's empty
builder, with no environment or file configuration sources. Existing builders
can register the transport with `builder.WebHost.UseIsoplane()`.

The factory builds the service provider and route conventions during pre-init.
Keep request-specific state out of that factory. Use an initialization hook for
asynchronous warmup. The .NET host and its hosted services start on the first
request, or during an explicit initialization warmup. Request scopes are created and disposed by ASP.NET's normal
request lifecycle. Ongoing `BackgroundService` work is rejected before startup:
its lifetime cannot extend beyond a completed dispatch. Asynchronous bounded
`IHostedService.StartAsync` work is supported.

To warm a read endpoint through middleware, binding and serialization before capture:

```csharp
Isoplane.Initialization.Initializing += () =>
    new IsoplaneApplication(application).WarmupAsync("/todos");
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
handlers can read and respond before the client finishes its upload. Upgrades
and direct file responses are unsupported.

Cancellation is checked before each upload read; it cannot interrupt a pending
host read or an individual response write awaiting acknowledgement. Disposing
an upload prevents new reads, while its host transport remains owned until the
dispatch settles. An early response releases unread content without background
draining. Streaming also does not yet impose a byte
cap on pending write payloads; that resource limit remains unresolved.

On Linux, use the repository's pinned .NET SDK, wasi-sdk and wasm-tools, then
run `dotnet publish -c Release`. The build restores the selected managed ASP.NET
runtime and request-delegate generator from NuGet; it does not borrow assemblies from the installed
shared framework. Unresolvable methods fail compilation. The PAL/import gate
and `isoplane doctor` then check the produced module.

Set `RuntimeFrameworkVersion` in the application project to select the ASP.NET
patch version (for example, `10.0.12`). Without an explicit selection, it uses the latest
patch listed by the .NET SDK. Ordinary `PackageReference` versions remain owned by
the application. Deployment bundles carry those resolved implementations;
the platform supplies its own WASM runtime and compiler.

To build local packages, pack `Isoplane.Sdk.csproj` and `Isoplane.AspNetCore.csproj` into
the same directory and configure that directory as a NuGet source. The fixture
builder at `guests/aspnet/build.sh` exercises this installation path with an
isolated consumer and package cache. Managed adapter tests run with
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

The generated project includes an ASP.NET route and the local build/run
configuration. Install the [.NET CLI](../../../tools/Isoplane.Cli/README.md)
and put its tool directory on your PATH. `isoplane dev` publishes and validates
the module, then serves it on localhost; source edits trigger a rebuild. This
path currently uses the NativeAOT compiler and a bundled Rust runtime, with
the Linux toolchain prerequisites above. Local state is private to the project directory.
