---
title: Handle requests without ASP.NET
---

An application that does not need ASP.NET can handle its requests in one static
method. `dotnet new isoplane-handler` creates a complete example with tests; this
page describes its parts.

## The project

The project uses the `Isoplane.Sdk` SDK and builds a library, with no
`Program.cs`:

```xml
<Project Sdk="Isoplane.Sdk/VERSION">
  <PropertyGroup>
    <Nullable>enable</Nullable>
    <ImplicitUsings>enable</ImplicitUsings>
  </PropertyGroup>
</Project>
```

## The handler

Mark the static method that receives every HTTP request with `[HttpHandler]`:

```csharp
using Isoplane;

public static class Handler
{
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) =>
        Task.FromResult(request.Path == "/"
            ? Response.Text("Hello from Isoplane!")
            : Response.Text("Not found", 404));
}
```

An application has exactly one. The build fails with `ISOPLANE1005` when there is
none, more than one, or one with another shape. The method returns
`Task<Response>` and takes a `Request`. It may take a second `Ctx` parameter, and
it may take a `StreamingRequest` in place of the `Request` to read a large body
as it arrives.

`Response.Stream` returns a writer for a response sent as it is produced. Pass
`request.Aborted` to work that should stop when the client goes away.

## Lifecycle methods

Two optional attributes mark static methods that return `Task` and take no
parameters. An application has at most one of each.

```csharp
[Initializer]
public static Task InitializeAsync() { ... }

[Activated]
public static Task ActivatedAsync() { ... }
```

`[Initializer]` runs during preparation, before Isoplane captures the template that every
instance starts from. Use it to load data and to run the code a request will run,
so no instance pays for its first use. `[Activated]` runs in each instance before
its first request. Use it for state that must differ between instances. See
[the application lifecycle](application-lifecycle.md).

## Settings

`RuntimeConfiguration.ReadValues()` returns the settings the platform supplies.
[Local development settings](local-settings.md) describes where they come from
while developing.
