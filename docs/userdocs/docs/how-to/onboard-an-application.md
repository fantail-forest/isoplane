---
title: Bring an existing ASP.NET application
---

An Isoplane application keeps its `Program.cs`. Isoplane supplies the server
transport; your application does not open a listener.

Keep an ordinary .NET project for code that also runs elsewhere, and reference it
from the Isoplane application project. Libraries still need to support the
sandbox's capabilities.

## Create the application project

Follow [the Quickstart's creation step](../quickstart.md#3-create-an-application) to make a `MyApi`
project next to your existing code. This supplies the matching SDK and launch
settings. Move your startup code into its `Program.cs`. If your application logic
is already in a separate library, add an ordinary project reference to that
library instead of copying its code.

Keep the existing project available while porting. Build and run a small route
first, then add the remaining services and routes and their integration tests.

## Adapt `Program.cs`

Two lines differ from an ordinary ASP.NET application:

```csharp
using Isoplane.AspNetCore;

var builder = IsoplaneHosting.CreateBuilder(args);
// Register application services here.

var app = builder.Build();
app.MapGet("/", () => "Hello from MyApi");

await app.RunAsync();
```

- `IsoplaneHosting.CreateBuilder(args)` replaces `WebApplication.CreateBuilder(args)`.
- The file ends with `await app.RunAsync()`. The synchronous `app.Run()` blocks
  until shutdown, which a sandboxed application cannot do. The editor marks the
  call as the error `ISOPLANE1020` and offers a quick fix that replaces it with
  `await app.RunAsync()`, making an explicit `void Main` asynchronous when there
  is one. A `RunAsync` whose task is discarded or blocked on is `ISOPLANE1021`,
  with the same fix.

Everything between stays as it was: service registrations before
`builder.Build()`, middleware and endpoints after it.

Isoplane runs `Program.cs` up to `RunAsync` during template preparation.
Preparation may retry; reusing a prepared template runs no initialization.
Every instance starts from the captured result. Settings, including
connection strings, are available while it runs. Two things do not carry over
into an instance: an open connection, so close any that start-up opens, and
uniqueness, so create a value that must differ between instances when an instance
activates. See [the application lifecycle](../concepts/application-lifecycle.md).

## Configuration

`IsoplaneHosting.CreateBuilder` reads `appsettings.json`, then
`appsettings.{Environment}.json`, then the settings the platform supplies. It does
not read your machine's environment variables. See
[local development settings](local-settings.md) for how to supply settings and
choose the environment while developing.

## Check application assumptions

Go through this list once for an existing application. Each item says what to
look for, how to find it, and what to do about it.

- **Blocking calls.** A synchronous wait such as `.Result`, `.Wait()`,
  `Thread.Sleep` or a synchronous database call throws in the sandbox. Build
  with the analyzer's dependency inspection on and fix each `ISOPLANE1003`.
  [Find and fix blocking calls](find-and-fix-blocking-calls.md) walks through
  it.
- **Threads.** Starting a thread throws, and the analyzer reports it as
  `ISOPLANE1002`. Rewrite the work as async code that the request awaits. See
  [threads and the thread pool](../concepts/dotnet-runtime-differences.md#thread-pool).
- **Long computations.** A method that computes for a long time without
  awaiting stalls its instance and can't be cancelled. Split it up as
  [running time](../concepts/execution-limits.md#running-time) describes.
- **Continuous and scheduled work.** A `BackgroundService` loop, a queue
  consumer or a job scheduler runs only while its instance exists and stops
  silently when the instance goes. Search for `BackgroundService`,
  `IHostedService` and scheduling packages. Drive the work from an endpoint
  instead, as [Run work on a schedule](run-work-on-a-schedule.md) shows. See
  [background work](../concepts/dotnet-runtime-differences.md#background-work).
- **WebSockets and HTTP/2.** Requests arrive as HTTP/1.1 without trailers,
  which rules out gRPC over HTTP/2. WebSockets are currently unavailable, and
  with them SignalR and Blazor Server interactivity. gRPC-Web, server-sent
  events and statically rendered Blazor pages work. See
  [incoming requests](../concepts/dotnet-runtime-differences.md#incoming-requests)
  and [SignalR and Blazor Server](../concepts/library-compatibility.md#signalr).
- **Data Protection keys.** Antiforgery tokens, the TempData cookie and
  authentication cookies use keys captured during initialization. Fresh
  initialization may create a different key; reusing a template retains its key.
  Keep earlier keys while protected data must remain readable.
  [Keep the keys across builds](keep-data-protection-keys.md). The
  [`isoplane-webapp`](../reference/project-templates.md#isoplane-webapp)
  template is set up this way.
- **MCP servers.** A server built with `ModelContextProtocol.AspNetCore` works
  over Streamable HTTP in its stateless mode. With session tracking on, the
  package registers a background service and the application refuses to
  start. The
  [`isoplane-mcpserver`](../reference/project-templates.md#isoplane-mcpserver)
  template is such a server.
- **Durable state.** Static fields, `IMemoryCache` and in-memory session
  state belong to one instance and can vanish at any time. Keep anything that
  must last in a database or another external service. See
  [instance state](../concepts/dotnet-runtime-differences.md#instance-state).
- **Database connections.** Every instance pools connections on its own, so
  a pool size in the application caps nothing overall, and the pool goes when
  its instance does. Cap connections with your provider's pooler, such as
  PgBouncer or a pooled endpoint, and open a connection per request.
  [Connect to PostgreSQL](connect-postgresql.md) shows this, with the
  driver's own pooling off.
- **Files.** The file system is read-only. Files in `wwwroot` are part of the
  application package, so serve them with `app.MapStaticAssets()` or
  `app.UseStaticFiles()`. For uploads and files the application generates, see
  [Handle uploads and generated files](handle-uploads-and-generated-files.md).
- **Native libraries.** A NuGet package that ships a native binary fails on
  the code path that calls it. Switch on `IsoplaneWarnOnNativeImports` to have
  the analyzer list such declarations, as
  [Control compatibility warnings](run-and-rebuild.md#control-compatibility-warnings)
  shows. See
  [native libraries](../concepts/dotnet-runtime-differences.md#native-libraries).
- **Framework assemblies.** A package that depends on an older version of an
  assembly ASP.NET Core provides gets the framework's copy, as in an ordinary
  ASP.NET Core project. Nothing to do.

[How .NET differs on Isoplane](../concepts/dotnet-runtime-differences.md) covers
everything else that differs from a server process, and
[library compatibility](../concepts/library-compatibility.md) lists the packages
and framework features known not to work, with what to use in their place.
