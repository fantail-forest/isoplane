# Isoplane templates

`dotnet new` templates for applications that run in Isoplane's WebAssembly
sandbox. Each generated file is commented as a guide to setting up a project of
its kind.

| Template | What it creates |
| --- | --- |
| `dotnet new isoplane-web` | An empty ASP.NET Core application |
| `dotnet new isoplane-webapi` | A Web API with Entity Framework Core, PostgreSQL, OpenAPI and tests |
| `dotnet new isoplane-mcpserver` | A Model Context Protocol server over HTTP, with example tools and tests |
| `dotnet new isoplane-handler` | A service that handles requests in one static method, without ASP.NET, with tests |
| `dotnet new isoplane-tests` | Sandbox tests for an application you already have |

The [Isoplane.Cli](https://www.nuget.org/packages/Isoplane.Cli) tool installs
the templates that match it:

```bash
dotnet tool install --global Isoplane.Cli --prerelease
isoplane setup
dotnet new isoplane-web --name MyApi
cd MyApi
dotnet run
```

Learn more at [isoplane.dev](https://isoplane.dev/).
