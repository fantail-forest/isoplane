---
title: Bring an existing ASP.NET application
---

An Isoplane application keeps its `Program.cs`. Isoplane supplies the server
transport; your application does not open a listener.

Keep an ordinary .NET project for code that also runs elsewhere, and reference it
from the Isoplane application project. Libraries still need to support the
sandbox's capabilities.

## Create the application project

Follow [the template creation steps](create-an-application.md#create-the-application) to make a `MyApi`
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
  until shutdown, which a sandboxed application cannot do, so the build rejects it
  with `ISOPLANE1005`.

Everything between stays as it was: service registrations before
`builder.Build()`, middleware and endpoints after it.

Isoplane runs `Program.cs` up to `RunAsync` during template preparation,
and every instance starts from the captured result. Preparation may retry;
reusing a prepared template runs no initialization. Do not open database
connections or create request-specific secrets at the top level of `Program.cs`;
see [the application lifecycle](application-lifecycle.md).

## Configuration

`IsoplaneHosting.CreateBuilder` reads `appsettings.json`, then
`appsettings.{Environment}.json`, then the settings the platform supplies. It does
not read your machine's environment variables. See
[local development settings](local-settings.md) for how to supply settings and
choose the environment while developing.

## Check application assumptions

Use asynchronous I/O and await request work before returning the response.

Isoplane suspends an application between requests, so background work cannot
continue after startup. An application whose `BackgroundService` is still running
once the host has started fails to start, with an error that names the service;
this includes services a library registers. An `IHostedService` may perform
bounded asynchronous startup work in `StartAsync`, and a `BackgroundService` that
finishes during startup is accepted.

Files in `wwwroot` are part of the application package. Serve them with
`app.MapStaticAssets()` or `app.UseStaticFiles()`.

Keep durable state outside the instance. Review
[the application lifecycle](application-lifecycle.md) for startup state,
connection pools, cancellation, and work after the response.
