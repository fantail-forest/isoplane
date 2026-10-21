# Isoplane ASP.NET Core

Run an ASP.NET Core application in Isoplane's WebAssembly sandbox. This package
is the project SDK for such an application. Routing, middleware, dependency
injection and configuration work as in any ASP.NET Core application; Isoplane
delivers requests to it, so it opens no listener.

## Start a project

```bash
dotnet tool install --global Isoplane.Cli --prerelease
isoplane setup
dotnet new isoplane-web --name MyApi
cd MyApi
dotnet run
```

`dotnet new isoplane-webapi` creates a larger Web API with Entity Framework Core,
PostgreSQL and tests, `dotnet new isoplane-webapp` a Razor Pages application on
the same database setup, and `dotnet new isoplane-mcpserver` a Model Context
Protocol server.

## The project

```xml
<Project Sdk="Isoplane.AspNetCore/VERSION">
  <PropertyGroup>
    <Nullable>enable</Nullable>
    <ImplicitUsings>enable</ImplicitUsings>
  </PropertyGroup>
</Project>
```

```csharp
using Isoplane.AspNetCore;

var builder = IsoplaneHosting.CreateBuilder(args);
var app = builder.Build();

app.MapGet("/", () => "Hello from ASP.NET on Isoplane");

await app.RunAsync();
```

Two lines differ from an ordinary ASP.NET Core application.
`IsoplaneHosting.CreateBuilder` takes the place of `WebApplication.CreateBuilder`,
and the entry point ends with `await app.RunAsync()`: the synchronous `app.Run()`
blocks, which a sandboxed application cannot do. The package's analyzers mark that
call as an error in your editor and offer the fix.

## What to know

- **Instances start from a template.** Isoplane runs `Program.cs` up to
  `RunAsync`, then the `Isoplane.Initialization.Initializing` callbacks, and
  captures the result. `app.WarmupAsync("/path")` in such a callback serves a
  request before the capture, so no instance pays for that route's first use.
- **Await work the response depends on.** Remaining work uses a bounded instance
  background allowance and may be lost if the instance is discarded. Continuous
  execution is unavailable: a `BackgroundService` still running once the host
  has started is refused by name.
- **Configuration is layered as in ASP.NET Core:** `appsettings.json`, then
  `appsettings.{Environment}.json`, then the settings the platform supplies. In
  local development those are User Secrets and the launch profile's environment
  variables.
- **Requests arrive as HTTP/1.1**, without connection upgrades or response
  trailers, so WebSockets are unavailable.

Keep the CLI, this package and the templates at the same version.

Learn more at [isoplane.dev](https://isoplane.dev/).
