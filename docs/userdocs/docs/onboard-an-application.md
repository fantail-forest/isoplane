---
title: Bring an existing ASP.NET application
---

Move your routes, middleware, and service registrations into an application
factory that returns a built `WebApplication`. Isoplane supplies the server
transport; your application does not open a listener.

Keep an ordinary .NET project for code that also runs elsewhere, and reference it
from the Isoplane application project. Libraries still need to support the
sandbox's capabilities and the selected compilation mode.

## Create the application wrapper

Follow [the template creation steps](create-an-application.md#create-the-application) to make a `MyApi`
project next to your existing code. This supplies the matching SDK, configurations
and local manifest. Move your startup registrations into its `Application.cs`.
If your application logic is already in a separate library, add an ordinary
project reference to that library instead of copying its code.

Keep the existing project available while porting. Build and run a small route
first, then add the remaining services and routes and their integration tests.

## Adapt application startup

This is the shape of the factory for an application named `MyApi`:

```csharp
using Microsoft.AspNetCore.Builder;
using Wisp.AspNetCore;

namespace MyApi;

public static class Application
{
    public static WebApplication Create()
    {
        var builder = WispHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        // Register application services here.
        var app = builder.Build();
        app.MapGet("/", () => "Hello from MyApi");
        return app;
    }
}
```

The generated project already selects `Wisp.AspNetCore` and sets
`WispApplicationFactory` to `MyApi.Application.Create`. If you change the namespace
or factory name, update that property to match. Keep package versions aligned
with your installed release.

Move service registrations before `builder.Build()` and middleware and endpoint
registrations after it. Return the application instead of calling `Run()` or
`RunAsync()`. The factory is called during preparation, before requests arrive.
Do not open database connections or create request-specific secrets there.

`WispHosting.CreateBuilder` starts without environment-variable or file-based
configuration providers. Do not assume that your development machine's
`appsettings.json`, environment, or filesystem is available inside the sandbox.

## Check application assumptions

Use asynchronous I/O and await request work before returning the response.
Long-running `BackgroundService` implementations are unsupported. An
`IHostedService` may perform bounded asynchronous startup work; that is different
from starting work that must continue after the response.

Keep durable state outside the instance. Review
[the application lifecycle](application-lifecycle.md) for startup state,
connection pools, cancellation, and work after the response.
