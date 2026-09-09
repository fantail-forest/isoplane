# WISP ASP.NET adapter

`Wisp.AspNetCore` runs ASP.NET routing, middleware and request-scoped dependency
injection over buffered WISP requests. It replaces the server transport and
generates the platform exports. The guest does not bind a listener.

Use the package's .NET SDK entry point so its compiler and target settings are
available to the first restore:

```xml
<Project Sdk="Wisp.AspNetCore/0.1.0">
  <PropertyGroup>
    <WispApplicationFactory>MyApp.Application.Create</WispApplicationFactory>
    <ImplicitUsings>enable</ImplicitUsings>
  </PropertyGroup>
</Project>
```

The named static method returns a built `WebApplication`:

```csharp
using Microsoft.AspNetCore.Builder;
using Wisp.AspNetCore;

namespace MyApp;

public static class Application
{
    public static WebApplication Create()
    {
        var builder = WispHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        var app = builder.Build();
        app.MapGet("/", () => "Hello from ASP.NET on WISP");
        return app;
    }
}
```

Register services and middleware as usual. `CreateBuilder` uses ASP.NET's empty
builder, with no environment or file configuration sources. Existing builders
can register the transport with `builder.WebHost.UseWisp()`.

The factory builds the service provider and route conventions during pre-init.
Keep database connections, production secrets and request-specific work out of
that factory. The .NET host and its hosted services start after activation, on
the first request. Request scopes are created and disposed by ASP.NET's normal
request lifecycle. Ongoing `BackgroundService` work is rejected before startup:
its lifetime cannot extend beyond a completed dispatch. Asynchronous bounded
`IHostedService.StartAsync` work is supported.

Responses preserve status, binary bodies and repeated headers. `OnStarting`
runs before headers become immutable; `OnCompleted` runs before the request
context is disposed, including error and cancellation paths. `RequestAborted`
receives WISP's dispatch cancellation wake and application-triggered aborts.
Streaming, upgrades and direct file responses are unsupported.

On Linux, use the repository's pinned .NET SDK, wasi-sdk and wasm-tools, then
run `dotnet publish -c Release`. The build restores a pinned managed ASP.NET
runtime closure from NuGet; it does not borrow assemblies from the installed
shared framework. Unresolvable methods fail compilation. The PAL/import gate
and `wisp doctor` then check the produced module.

To build local packages, pack `Wisp.Sdk.csproj` and `Wisp.AspNetCore.csproj` into
the same directory and configure that directory as a NuGet source. The fixture
builder at `guests/aspnet/build.sh` exercises this installation path with an
isolated consumer and package cache. Managed adapter tests run with
`dotnet test guest-sdks/dotnet/AspNetCore.Tests`.

## Start from a template

Pack `Templates/Wisp.Templates.csproj` alongside the two SDK packages in your
local NuGet feed. Install that template package and generate an application:

```sh
dotnet new install /path/to/feed/Wisp.Templates.0.1.0.nupkg
dotnet new isoplane-web --name MyApp --tenant my-local-tenant
cd MyApp
dotnet nuget add source /path/to/feed --name wisp-local --configfile nuget.config
isoplane dev
```

The generated project includes an ASP.NET route and the local build/run
configuration. Install the [.NET CLI](../../../tools/Isoplane.Cli/README.md)
and put its tool directory on your PATH. `isoplane dev` publishes and validates
the module, then serves it on localhost; source edits trigger a rebuild. This
path currently uses the NativeAOT compiler and a bundled Rust runtime, with
the Linux toolchain prerequisites above. The tenant argument explicitly scopes
local state; it is not a hosted credential or an authorization claim.
