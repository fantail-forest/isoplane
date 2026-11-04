---
title: Library compatibility
---

Most NuGet packages run on Isoplane unchanged. The ones that fail or require
workarounds do so for a handful of reasons, all of which follow from
[how .NET differs on Isoplane](dotnet-runtime-differences.md): something waits
synchronously, starts a thread, requires continuous background execution,
writes a file, loads a native library, or keeps in one instance's memory what
the whole application needs. Refer to that page to understand these limits
deeply.

This page lists the packages and framework features we know about: the ones
that work unchanged, and the ones that fail even though your own code is
asynchronous.

It does not list synchronous methods whose whole remedy is to call their
asynchronous counterpart, such as Entity Framework Core's
[`SaveChanges`][DbContext.SaveChanges], which becomes
[`SaveChangesAsync`][DbContext.SaveChangesAsync], or a task read with
[`Task.Result`][Task.Result], which is awaited instead. Most packages have
some, and one rule covers them all:
[Use async, not blocking waits](dotnet-runtime-differences.md#blocking-waits).

For each one that fails, the entry says what fails and how to work around it
today, with the size of that change. Where there is more than one way through,
the entry says which we recommend and gives the other as an alternative. A
package that is not listed has not been checked; to look for likely problems
before relying on it, [check it yourself](#check-a-package).

## At a glance {#summary}

| Package                                                           | API or feature                                                                                                                                                           | Status         | What to do                                                                                                       |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------- | ---------------------------------------------------------------------------------------------------------------- |
| ASP.NET Core                                                      | Minimal APIs, MVC controllers and views, Razor Pages, model validation, antiforgery tokens, TempData in a cookie, output caching, response compression and health checks | Works          | Nothing                                                                                                          |
| ASP.NET Core                                                      | [Session over a distributed cache, and TempData kept in it](#session)                                                                                                    | Needs a change | Small change: call `UseLoadedSession` in place of `UseSession`                                                   |
| ASP.NET Core                                                      | [File uploads over 64 KB](#uploads)                                                                                                                                      | Needs a change | Moderate change: read the upload as a stream. Or hold small uploads in memory                                    |
| ASP.NET Core                                                      | [`Request.EnableBuffering()` over 30 KB](#enable-buffering)                                                                                                              | Needs a change | Moderate change: read the body once, as a stream. Or buffer up to a limit                                        |
| ASP.NET Core                                                      | [Minimal API handlers that use source-generated code](#generated-code-in-handlers)                                                                                       | Needs a change | Small change: register the JSON context and return objects. Or switch one generator off                          |
| ASP.NET Core                                                      | [Data Protection key stores](#data-protection)                                                                                                                           | Needs a change | Small change: use the replacement Isoplane provides                                                              |
| ASP.NET Core                                                      | [XML formatters over 30 KB](#xml-formatters)                                                                                                                             | Needs a change | Moderate change: use the asynchronous XML APIs. Or buffer up to a limit                                          |
| ASP.NET Core                                                      | [Newtonsoft.Json formatters over 30 KB](#newtonsoft-json)                                                                                                                | Needs a change | Moderate to large change: move to `System.Text.Json`. Or keep Newtonsoft.Json and raise two limits               |
| ASP.NET Core                                                      | [Rate limiters that count in memory, including the built-in ones](#rate-limiting)                                                                                        | Needs a change | Moderate change: use a limiter that counts in a shared store. Or limit requests in front of the application      |
| ASP.NET Core                                                      | [Validation attributes, localizers, token callbacks and service factories that do I/O](#synchronous-extension-points)                                                    | Needs a change | Moderate change: move the I/O elsewhere                                                                          |
| ASP.NET Core                                                      | [WebSockets](#connections-and-background-work)                                                                                                                           | Not supported  | No workaround                                                                                                    |
| ASP.NET Core                                                      | [SignalR and Blazor Server interactivity](#signalr)                                                                                                                      | Not supported  | No workaround: SignalR needs sticky sessions, which Isoplane does not keep |
| ASP.NET Core                                                      | [gRPC over HTTP/2](#connections-and-background-work)                                                                                                                     | Needs a change | Moderate change: use gRPC-Web                                                                                    |
| ASP.NET Core                                                      | [Continuously running `BackgroundService`, jobs and queue consumers](#connections-and-background-work)                                                                   | Needs a change | Large change: do the work during a request                                                                       |
| Microsoft.AspNetCore.Identity.EntityFrameworkCore                 | Creating users, password sign-in and the sign-in cookie                                                                                                                  | Works          | Nothing                                                                                                          |
| Microsoft.AspNetCore.Authentication.JwtBearer                     | Validating tokens against signing keys fetched from `MetadataAddress`                                                                                                    | Works          | Nothing                                                                                                          |
| Microsoft.AspNetCore.Authentication.OpenIdConnect                 | The sign-in redirect, which fetches the provider's metadata                                                                                                              | Works          | Nothing                                                                                                          |
| Swashbuckle.AspNetCore                                            | Generating the OpenAPI document                                                                                                                                          | Works          | Nothing                                                                                                          |
| Microsoft.Extensions.Configuration                                | [`IConfigurationProvider` implementations that load from a remote service, such as Azure Key Vault](#remote-configuration)                                               | Needs a change | Moderate change: fetch fixed values before `builder.Build()`, and read changing ones where they are used         |
| Microsoft.Extensions.Logging.Console                              | [The console logger in an ASP.NET Core application](#console-logging)                                                                                                    | Works          | Nothing                                                                                                          |
| Microsoft.Extensions.Logging.Console                              | [The console logger without ASP.NET Core, or with another dependency injection container](#console-logging)                                                              | Needs a change | Small change: log through Isoplane's provider                                                                    |
| Serilog.AspNetCore                                                | `UseSerilog` with the console sink                                                                                                                                       | Works          | Nothing                                                                                                          |
| Entity Framework Core                                             | Asynchronous queries, saves, transactions and migrations, the retrying execution strategy, and context pooling                                                           | Works          | Nothing                                                                                                          |
| Entity Framework Core                                             | [Lazy loading](#lazy-loading)                                                                                                                                            | Needs a change | Moderate change: load with `Include`                                                                             |
| Entity Framework Core                                             | [`TransactionScope`](#transaction-scope)                                                                                                                                 | Needs a change | Small change: use a database transaction                                                                         |
| Microsoft.Extensions.Diagnostics.HealthChecks.EntityFrameworkCore | `AddDbContextCheck`                                                                                                                                                      | Works          | Nothing                                                                                                          |
| Npgsql                                                            | PostgreSQL, directly or through Entity Framework Core or Dapper                                                                                                          | Works          | Nothing. See [Connect PostgreSQL](../how-to/connect-postgresql.md) for pooling                                   |
| Dapper                                                            | Asynchronous queries and commands                                                                                                                                        | Works          | Nothing                                                                                                          |
| MySqlConnector                                                    | Connecting to MySQL and running queries                                                                                                                                  | Works          | Nothing                                                                                                          |
| SqlClient                                                         | [SQL Server, including its Entity Framework Core provider](#sql-server)                                                                                                  | Not supported  | No workaround yet: the driver waits synchronously                                                                |
| Sqlite                                                            | [SQLite, including its Entity Framework Core provider](#sqlite)                                                                                                          | Needs a change | Small change for read-only data loaded into each instance. A database the application writes to is not supported |
| MongoDB.Driver                                                    | [MongoDB](#mongodb)                                                                                                                                                      | Not supported  | Not for most deployments: waits on the driver. A load-balanced deployment works with two settings                |
| StackExchange.Redis                                               | [StackExchange.Redis 2.x, and the Redis cache, output cache and other packages that bring it in](#redis)                                                                 | Needs a change | Small change: reference the stable 3.x client and call it asynchronously                                         |
| Microsoft.Extensions.Caching.Hybrid                               | `HybridCache`                                                                                                                                                            | Works          | Nothing for the in-memory level. A Redis second level needs the StackExchange.Redis change above                 |
| Microsoft.Extensions.Http.Resilience                              | `AddStandardResilienceHandler`                                                                                                                                           | Works          | Nothing                                                                                                          |
| OpenTelemetry                                                     | The SDK, and instrumentation such as ASP.NET Core's                                                                                                                      | Works          | Nothing                                                                                                          |
| OpenTelemetry                                                     | [Exporters that send over the network, including OTLP](#opentelemetry)                                                                                                   | Needs a change | Small change, traces only: use the exporter Isoplane provides                                                    |
| ModelContextProtocol.AspNetCore                                   | [A stateless MCP server over Streamable HTTP](#mcp-sessions)                                                                                                             | Works          | Nothing                                                                                                          |
| ModelContextProtocol.AspNetCore                                   | [An MCP server that tracks sessions](#mcp-sessions)                                                                                                                      | Needs a change | Small change: use stateless mode                                                                                 |
| FluentValidation                                                  | Synchronous rules, and asynchronous ones through `ValidateAsync`                                                                                                         | Works          | Nothing                                                                                                          |
| MediatR                                                           | `IMediator.Send`                                                                                                                                                         | Works          | Nothing                                                                                                          |
| AutoMapper                                                        | Mapping objects                                                                                                                                                          | Works          | Nothing                                                                                                          |

A linked row has a section below with the details.

<details>
<summary>How the statuses and changes are described</summary>

- **Works** means the package or feature runs as it does anywhere else, with no
  change.
- **Needs a change** means it fails as written, and the row says what to change.
- **Not supported** means there is no way through, or none for most
  applications.

The changes are sized like this:

- **Small** is a one-for-one replacement: the asynchronous form of the same
  call, a setting, or a method Isoplane provides.
- **Moderate** is some restructuring, or a few lines of your own.
- **Large** is a different library or a different approach.

</details>

## Recognize a failure {#symptoms}

Exception messages from the .NET runtime itself are short identifiers, not
sentences, so read the stack trace the log prints with the failure to see which
call failed.

| What you see                                                                                                                                                                                                                                                                   | What it means                                                                                                                                                                                                           |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PlatformNotSupportedException: Arg_PlatformNotSupported`, with `Task.InternalWait` (internal to [`Task`][Task]), [`SemaphoreSlim.Wait`][SemaphoreSlim.Wait], [`Thread.Sleep`][Thread.Sleep], [`Socket`][Socket] or [`NetworkStream`][NetworkStream] near the top of the stack | Something waited synchronously. If the call is in your own code, see [Use async, not blocking waits](dotnet-runtime-differences.md#blocking-waits). If it is inside a library, find the library in the table above.                   |
| `PlatformNotSupportedException: net_wasi_sync_name_resolution`                                                                                                                                                                                                                 | The same, for a connection that was opened synchronously.                                                                                                                                                               |
| `PlatformNotSupportedException: Arg_PlatformNotSupported`, with [`Thread.Start`][Thread.Start] near the top of the stack                                                                                                                                                       | A library started a thread. See [MongoDB](#mongodb), [Redis](#redis) and [the console logger](#console-logging).                                                                                                        |
| `DirectoryNotFoundException: /tmp/`                                                                                                                                                                                                                                            | ASP.NET Core tried to move a large request or response to a temporary file. See [uploads](#uploads), [request buffering](#enable-buffering), [Newtonsoft.Json](#newtonsoft-json) and [XML formatters](#xml-formatters). |
| `IOException: Read-only file system`                                                                                                                                                                                                                                           | Something tried to write a file. The [file system is read-only](dotnet-runtime-differences.md#file-system).                                                                                                             |
| [`DllNotFoundException`][DllNotFoundException], sometimes inside a `TypeInitializationException`                                                                                                                                                                               | A package called into a native library that the runtime does not include. An application cannot bring its own; see [native libraries](dotnet-runtime-differences.md#native-libraries).                                  |
| The application refuses to start with `unhandled System.PlatformNotSupportedException` and no stack trace                                                                                                                                                                      | A call made while the application starts failed. The known ones are [remote configuration](#remote-configuration) and, in an application written without ASP.NET Core, [the console logger](#console-logging).          |
| An MCP server answers some requests with 404 and `Session not found`                                                                                                                                                                                                           | The server tracks sessions, and the request reached an instance that does not hold its session. See [MCP sessions](#mcp-sessions).                                                                                      |
| A minimal API endpoint answers 200 with an empty body, or the build fails with `CS9144: Cannot intercept method`                                                                                                                                                               | A handler names code that another source generator produces. See [generated code in handlers](#generated-code-in-handlers).                                                                                             |
| `InvalidOperationException: The session could not be loaded from its cache`                                                                                                                                                                                                    | The session was used before it was loaded. See [session](#session).                                                                                                                                                     |

## ASP.NET Core

These ship with the Isoplane SDK as part of ASP.NET Core, so they need no
package reference, except the Newtonsoft.Json formatters, which come from a
package of their own.

### Session over a distributed cache {#session}

With [`AddSession`][AddSession], [`UseSession`][UseSession] and an
[`IDistributedCache`][IDistributedCache] that reaches a database or another
service, the first read or write of [`HttpContext.Session`][HttpContext.Session]
in a request loads the session synchronously, and the load fails. The read or
write throws `InvalidOperationException: The session could not be loaded from
its cache`, and the request fails. The cache's own exception is logged as
`Session cache read exception`, under the category
`Microsoft.AspNetCore.Session.DistributedSession`.

```csharp
app.UseSession();
app.MapPost("/cart/items", (HttpContext context, string item) =>
{
    var cart = context.Session.GetString("cart");   // throws: the session could not be loaded
    context.Session.SetString("cart", $"{cart},{item}");
});
```

[TempData][Controller.TempData] fails the same way when it is kept in the
session with MVC's
[`AddSessionStateTempDataProvider`][AddSessionStateTempDataProvider].

The in-memory cache, [`AddDistributedMemoryCache`][AddDistributedMemoryCache],
does not fail this way, but it does not keep a session for the application
either. Each session lives in the memory of the instance that wrote it.
Successive requests can reach different instances, and an instance
[can be discarded after any request](dotnet-runtime-differences.md#ephemeral-instances),
so a visitor's session is missing whenever a request reaches another instance.

Typically used for sign-in flows, shopping carts, multi-step forms, and messages
shown after a redirect.

#### Workaround: call `UseLoadedSession`

Call [`UseLoadedSession`][UseLoadedSession], from the ASP.NET Core adapter
library `Isoplane.AspNetCore`, in place of `UseSession`. It adds the same
session middleware and loads each request's session asynchronously before the
rest of the pipeline runs:

```csharp
app.UseStaticFiles();
app.UseLoadedSession();   // in place of app.UseSession()
```

Every request that passes through it reads the cache once, whether or not it
uses the session, so place it after middleware that answers requests without
one, such as `UseStaticFiles`.

The Redis cache works too, with the client version given under [Redis](#redis).

Size of the change: small. One call, replaced once.

### File uploads over 64 KB {#uploads}

When ASP.NET Core reads a form, through
[`HttpRequest.ReadFormAsync`][HttpRequest.ReadFormAsync] or an
[`IFormFile`][IFormFile] parameter, it keeps the first 64 KB of an uploaded
file in memory and moves a larger one to a temporary file. There is nowhere to
write one, so `ReadFormAsync` throws `DirectoryNotFoundException: /tmp/`. A
minimal API handler that takes an `IFormFile` parameter answers 400 with an
empty body instead.

```csharp
app.MapPost("/documents", async (HttpContext context, DocumentStore store) =>
{
    var form = await context.Request.ReadFormAsync();   // throws for a file over 64 KB
    var file = form.Files["document"]!;
    await using var content = file.OpenReadStream();
    await store.SaveAsync(file.FileName, content);
});
```

Typically used for any multipart upload: images, documents, imports. Large
fields of an ordinary URL-encoded form are not affected.

#### Recommended workaround: read each file as a stream

Read each file while it arrives, with [`MultipartReader`][MultipartReader].
Isoplane hands the request body to the application as it reads, so a file of
any size passes through in small pieces and is never held in memory.

```csharp
using Microsoft.AspNetCore.WebUtilities;
using Microsoft.Net.Http.Headers;

app.MapPost("/documents", async (HttpContext context, DocumentStore store) =>
{
    var boundary = HeaderUtilities.RemoveQuotes(
        MediaTypeHeaderValue.Parse(context.Request.ContentType).Boundary).Value!;
    var reader = new MultipartReader(boundary, context.Request.Body);

    while (await reader.ReadNextSectionAsync(context.RequestAborted) is { } section)
    {
        // FileStream yields the file as it arrives. Pass it straight on, for
        // example as the content of an HttpClient request to object storage.
        if (section.AsFileSection() is { } file)
            await store.SaveAsync(file.FileName, file.FileStream!, context.RequestAborted);
    }
});
```

[`AsFileSection`][AsFileSection] returns null for a part that is not a file.

Size of the change: moderate. The handler reads the multipart body itself
instead of taking an `IFormFile`.

#### Alternative workaround: keep `IFormFile` and hold uploads in memory

Raising [`MemoryBufferThreshold`][FormOptions.MemoryBufferThreshold] in
[`FormOptions`][FormOptions] gets `IFormFile` working as written, by holding
each upload in memory. We do not recommend it wherever the upload can be read
as a stream: every upload then counts toward the instance's [memory limit](resource-limits.md#memory), and the
largest file the application can accept is whatever memory it has left. If you
do use it, set
[`MultipartBodyLengthLimit`][FormOptions.MultipartBodyLengthLimit] to the same
size, so that a larger file is refused instead of buffered.

```csharp
using Microsoft.AspNetCore.Http.Features;

const int LargestUpload = 4 * 1024 * 1024;

builder.Services.Configure<FormOptions>(options =>
{
    options.MemoryBufferThreshold = LargestUpload;      // kept in memory up to this size
    options.MultipartBodyLengthLimit = LargestUpload;   // a larger file is refused
});
```

Size of the change: small.

### `Request.EnableBuffering()` over 30 KB {#enable-buffering}

[`HttpRequest.EnableBuffering`][EnableBuffering] lets a request body be read more than
once. It keeps 30 KB in memory and moves a larger body to a temporary file,
which fails with `DirectoryNotFoundException: /tmp/` when the body is read.

```csharp
context.Request.EnableBuffering();
var payload = await new StreamReader(context.Request.Body).ReadToEndAsync();   // throws over 30 KB
context.Request.Body.Position = 0;   // so that the body can be bound afterwards
```

Typically used for verifying a webhook signature before binding the body, and
for logging request bodies.

#### Recommended workaround: read the body once

Read the body once, as a stream, and do both jobs in that pass. For a signed
webhook, wrap the body in a [`CryptoStream`][CryptoStream] that computes the
signature while the payload is deserialized from it:

```csharp
using var hmac = new HMACSHA256(signingKey);
Order? order;
await using (var hashing = new CryptoStream(context.Request.Body, hmac, CryptoStreamMode.Read, leaveOpen: true))
{
    order = await JsonSerializer.DeserializeAsync<Order>(hashing, cancellationToken: context.RequestAborted);
    await hashing.CopyToAsync(Stream.Null, context.RequestAborted);   // read to the end, so the hash is complete
}

var expected = Convert.FromHexString(context.Request.Headers["X-Signature"].ToString());
if (!CryptographicOperations.FixedTimeEquals(hmac.Hash!, expected))
    return Results.Unauthorized();

// Only now act on the order.
```

Nothing is held in memory but the object being built. The payload is parsed
before its signature is known, so do nothing with it until the comparison has
passed.

Size of the change: moderate.

#### Alternative workaround: buffer up to a size you choose

Pass the largest body you accept as both the threshold and the limit. A body
up to that size stays in memory, and a larger one is refused.

```csharp
const int LargestBody = 1024 * 1024;

context.Request.EnableBuffering(bufferThreshold: LargestBody, bufferLimit: LargestBody);
```

This holds the whole body for the length of the request, counted toward the
instance's [memory limit](resource-limits.md#memory), so prefer the single pass where it fits.

Size of the change: small.

### Minimal API handlers that use source-generated code {#generated-code-in-handlers}

Isoplane builds every application with ASP.NET Core's
[request delegate generator][rdg] switched on, which writes the code for each
minimal API endpoint when the application builds. One source generator cannot
see what another produces, so a handler lambda that names generated members,
such as the properties of a `System.Text.Json` serializer context, is misread.
The endpoint below answers 200 with an empty body and reports no error. A
handler of the same kind that returns nothing fails to build instead, with
`CS9144: Cannot intercept method`.

```csharp
app.MapPost("/orders", async (HttpContext context) =>
{
    var order = await JsonSerializer.DeserializeAsync(context.Request.Body, AppJsonContext.Default.Order);
    return Results.Json(new OrderSummary(order!), AppJsonContext.Default.OrderSummary);   // 200, empty body
});
```

Typically seen in applications that use `System.Text.Json` source generation
and pass its type information by hand. ASP.NET Core itself switches the
generator on only for native AOT publishing, so this code works in an ordinary
ASP.NET Core application.

#### Recommended workaround: register the context and let ASP.NET Core serialize

Register the context once, with
[`ConfigureHttpJsonOptions`][ConfigureHttpJsonOptions], and have handlers take
and return objects. ASP.NET Core then uses the generated code without the
handler naming it.

```csharp
builder.Services.ConfigureHttpJsonOptions(options =>
    options.SerializerOptions.TypeInfoResolverChain.Insert(0, AppJsonContext.Default));

app.MapPost("/orders", (Order order) => new OrderSummary(order));
```

Size of the change: small.

#### Alternative workaround: switch the generator off

Set `<EnableRequestDelegateGenerator>false</EnableRequestDelegateGenerator>`
in the project file. ASP.NET Core then works out each endpoint's code when the
application starts, which on Isoplane happens once, during
[initialization](application-lifecycle.md#initialization).

Size of the change: small.

### Data Protection key stores {#data-protection}

Data Protection reads and writes its keys through a synchronous interface,
[`IXmlRepository`][IXmlRepository], so a store that keeps them in a database or
another service cannot work.
[`PersistKeysToDbContext`][PersistKeysToDbContext] fails when the keys are
first read: ASP.NET Core logs `An error occurred while reading the key ring`,
and protecting data then throws
[`CryptographicException`][CryptographicException].
[`PersistKeysToFileSystem`][PersistKeysToFileSystem] fails with
`Read-only file system`.

```csharp
builder.Services.AddDataProtection().PersistKeysToDbContext<AppDbContext>();
```

Typically used for keeping authentication cookies and antiforgery tokens valid
after a new build.

#### Workaround: use the key stores Isoplane provides

Isoplane provides replacements, extension methods on
[`IDataProtectionBuilder`][IDataProtectionBuilder] described in
[Keep ASP.NET Core Data Protection keys across builds](../how-to/keep-data-protection-keys.md).

- [`UseKeysFromDbContext`](../reference/sdk/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/DataProtectionDbContextKeys/UseKeysFromDbContext.md)
  takes the place of `PersistKeysToDbContext` and uses the same table. It is in
  the [`Isoplane.AspNetCore.DataProtection.EntityFrameworkCore`](../reference/sdk/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore.md)
  package, which brings in
  [`Microsoft.AspNetCore.DataProtection.EntityFrameworkCore`][nuget-dp-ef].
- [`UseConfiguredKeys`](../reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/DataProtectionKeys/UseConfiguredKeys.md)
  derives the keys from a secret in the application's settings.
- [`UseKeysFrom`](../reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/DataProtectionKeys/UseKeysFrom.md)
  serves any other store.

The last two are in Isoplane's ASP.NET Core adapter library,
`Isoplane.AspNetCore`, which every Isoplane ASP.NET Core project already
references.

Size of the change: small for Entity Framework Core, where one package
reference is added and one call replaces another. Moderate for another store, where you supply the two
functions that read and add keys.

### XML formatters over 30 KB {#xml-formatters}

With [`AddXmlSerializerFormatters`][AddXmlSerializerFormatters], MVC moves an
XML request or response larger than about 30 KB to a temporary file, which
fails with `DirectoryNotFoundException: /tmp/`.

Typically used for APIs that accept or return XML. JSON through
[`System.Text.Json`][System.Text.Json] has no such limit.

#### Recommended workaround: read and write XML with the asynchronous XML APIs

These stream the body instead of holding it. To read, use an
[`XmlReader`][XmlReader] created with
[`Async = true`][XmlReaderSettings.Async], or
[`XDocument.LoadAsync`][XDocument.LoadAsync]. To write, use an
[`XmlWriter`][XmlWriter] created the same way, or
[`XDocument.SaveAsync`][XDocument.SaveAsync].

```csharp
app.MapPost("/orders", async (HttpContext context) =>
{
    using var reader = XmlReader.Create(context.Request.Body, new XmlReaderSettings { Async = true });
    while (await reader.ReadAsync())
    {
        // Handle each node as it arrives.
    }
});
```

Size of the change: moderate. The handler reads and writes the XML itself
instead of leaving it to MVC.

#### Alternative workaround: buffer a body that is bound with `XmlSerializer`

[`XmlSerializer`][XmlSerializer] reads synchronously, so a body bound to a
class with it has to be in memory first. Refuse a body over a size you choose,
copy the rest into a `MemoryStream`, and deserialize from that:

```csharp
const int LargestBody = 1024 * 1024;

if (context.Request.ContentLength is null or > LargestBody)
    return Results.StatusCode(StatusCodes.Status413PayloadTooLarge);

using var buffer = new MemoryStream();
await context.Request.Body.CopyToAsync(buffer, context.RequestAborted);
buffer.Position = 0;
var order = (Order)new XmlSerializer(typeof(Order)).Deserialize(buffer)!;
```

The whole body is then held in memory, counted toward the instance's [memory limit](resource-limits.md#memory).

Size of the change: small to moderate.

### Newtonsoft.Json formatters over 30 KB {#newtonsoft-json}

With [`AddNewtonsoftJson`][AddNewtonsoftJson], from the
[`Microsoft.AspNetCore.Mvc.NewtonsoftJson`][nuget-newtonsoft] package, MVC
moves a JSON request or response larger than about 30 KB to a temporary file,
which fails with `DirectoryNotFoundException: /tmp/`.

Typically used by applications that keep Newtonsoft.Json for its converters or
for compatibility with existing clients. There are two ways through, and the
second keeps Newtonsoft.Json.

#### Recommended workaround: `System.Text.Json` with source generation

Remove the call to `AddNewtonsoftJson`, so that MVC uses its default JSON
formatters, which are built on [`System.Text.Json`][System.Text.Json]. They
read and write the body as a stream and keep no copy of it, whatever its size.

Then add a [source-generated context][stj-source-generation] for the types the
API exchanges. The serialization code is written when the application builds,
on your machine, so the running application does not work it out by
reflection.

```csharp
[JsonSerializable(typeof(Order))]
[JsonSerializable(typeof(OrderSummary))]
internal sealed partial class AppJsonContext : JsonSerializerContext;

// For controllers:
builder.Services.AddControllers().AddJsonOptions(options =>
    options.JsonSerializerOptions.TypeInfoResolverChain.Insert(0, AppJsonContext.Default));

// For minimal APIs:
builder.Services.ConfigureHttpJsonOptions(options =>
    options.SerializerOptions.TypeInfoResolverChain.Insert(0, AppJsonContext.Default));
```

The two registrations are [`AddJsonOptions`][AddJsonOptions] and
[`ConfigureHttpJsonOptions`][ConfigureHttpJsonOptions]. Let ASP.NET Core do the
serializing from there: a minimal API handler that names the context's members
itself runs into
[another entry on this page](#generated-code-in-handlers).

Size of the change: moderate, and large for an application with many
Newtonsoft.Json converters or attributes, which have to be rewritten for
`System.Text.Json`.

#### Alternative workaround: keep Newtonsoft.Json and raise its buffers

On `MvcNewtonsoftJsonOptions`, raise
[`InputFormatterMemoryBufferThreshold`][InputFormatterMemoryBufferThreshold]
and
[`OutputFormatterMemoryBufferThreshold`][OutputFormatterMemoryBufferThreshold]
to the largest body you expect.

```csharp
const int LargestBody = 1024 * 1024;

builder.Services.AddControllers().AddNewtonsoftJson(options =>
{
    options.InputFormatterMemoryBufferThreshold = LargestBody;
    options.OutputFormatterMemoryBufferThreshold = LargestBody;
});
```

A body up to that size is then held in memory in full, counted toward the
instance's [memory limit](resource-limits.md#memory), and a larger one fails as before.

Size of the change: small.

### Rate limiters that count in memory {#rate-limiting}

The rate limiters ASP.NET Core provides, registered with
[`AddFixedWindowLimiter`][AddFixedWindowLimiter],
[`AddSlidingWindowLimiter`][AddSlidingWindowLimiter],
[`AddTokenBucketLimiter`][AddTokenBucketLimiter] or
[`AddConcurrencyLimiter`][AddConcurrencyLimiter], keep their counts in the
memory of each instance, so their limits do not hold for the application.
Isoplane runs as many instances as the traffic needs, so a client can make the
full number of requests at every instance its requests reach. The more traffic
the application has, the more instances share it, and the looser the limit
gets. A new instance also starts its count from nothing, because
[memory belongs to one instance](dotnet-runtime-differences.md#ephemeral-instances).

```csharp
builder.Services.AddRateLimiter(options =>
    options.AddFixedWindowLimiter("api", limiter =>
    {
        limiter.PermitLimit = 100;   // 100 a minute for each instance, not for the application
        limiter.Window = TimeSpan.FromMinutes(1);
    }));

var app = builder.Build();
app.UseRateLimiter();
app.MapGet("/orders", () => Results.Ok()).RequireRateLimiting("api");
```

A sliding-window limiter is also late to give permits back after an idle
spell. [`FixedWindowRateLimiter`][FixedWindowRateLimiter] and
[`TokenBucketRateLimiter`][TokenBucketRateLimiter] work out what to refill from
the time that has passed. A sliding window instead moves on one segment for
each tick of the middleware's 100 ms timer, which runs
[between requests](dotnet-runtime-differences.md#background-work) and can be
delayed by throttling.

Typically used for limiting how often a client may call an endpoint.

#### Recommended workaround: use a limiter that counts in a shared store

[`UseRateLimiter`][UseRateLimiter] takes any limiter, and one that keeps its
counts in a store every instance shares, such as Redis, holds for the
application. Choose one that does no I/O when it is asked for a permit
synchronously. The middleware first calls each limiter's
[`AttemptAcquire`][RateLimiter.AttemptAcquire], and awaits
[`AcquireAsync`][RateLimiter.AcquireAsync] only when that grants no permit, so
a limiter that reaches its store from `AttemptAcquire`
[waits synchronously](dotnet-runtime-differences.md#blocking-waits) and fails.

The limiters in the [`RedisRateLimiting`][nuget-redis-rate-limiting] package
are built this way: their `AttemptAcquire` grants nothing, and they count in
`AcquireAsync`. The package brings in the 2.x Redis client, so reference the
3.x client as described under [Redis](#redis).

Size of the change: moderate. The limiter changes, and the application needs a
store every instance can reach.

#### Alternative workaround: limit requests in front of the application

Enforce the limit where every request passes, such as the CDN, API gateway or
reverse proxy in front of the application.

Size of the change: large. Limiting moves out of the application.

### Validation attributes, localizers, token callbacks and service factories that do I/O {#synchronous-extension-points}

Some extension points are synchronous by definition, so an implementation that
queries a database or calls a service has to block, and throws
[`PlatformNotSupportedException`][PlatformNotSupportedException]:

- [`ValidationAttribute.IsValid`][ValidationAttribute.IsValid] and
  [`IValidatableObject.Validate`][IValidatableObject.Validate], for example a
  "this name is not taken" check.
- [`IStringLocalizer`][IStringLocalizer], when the strings come from a
  database.
- [`TokenValidationParameters`][TokenValidationParameters] callbacks such as
  [`IssuerSigningKeyResolver`][IssuerSigningKeyResolver], when they fetch
  signing keys.
- Constructors and dependency injection factories that connect when the object
  is created.

```csharp
public sealed class UniqueEmailAttribute : ValidationAttribute
{
    protected override ValidationResult? IsValid(object? value, ValidationContext context)
    {
        var db = context.GetRequiredService<ShopDb>();
        return db.Customers.Any(customer => customer.Email == (string?)value)   // a synchronous query: throws
            ? new ValidationResult("That email address is already registered.")
            : ValidationResult.Success;
    }
}
```

#### Workaround: move the I/O to where it can be awaited

Each of the four has its own way out:

- **Validation.** Run the check in the handler, or with a validator that has
  an asynchronous form. FluentValidation's
  [`MustAsync`][fluentvalidation-async] rules run with
  [`ValidateAsync`][fluentvalidation-async].
- **Localized strings and other lookup data.** Load them asynchronously in an
  [`Initialization.Initializing`](../reference/sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing.md)
  callback and serve them from memory. What initialization leaves in memory is
  copied into every instance.
- **Signing keys.** Set
  [`JwtBearerOptions.Authority`][JwtBearerOptions.Authority] or
  [`JwtBearerOptions.MetadataAddress`][JwtBearerOptions.MetadataAddress]. The
  handler then fetches and refreshes the keys asynchronously.
- **Services.** Construct the object without connecting, and connect
  asynchronously the first time it is used.

Size of the change: moderate. The I/O moves to a place that can await it.

### SignalR and Blazor Server {#signalr}

SignalR, and Blazor Server interactivity, which is built on it, are currently
unsupported. SignalR keeps each connection's state in the memory of the server
that accepted it, and
[requires that server to handle every request of the connection][signalr-sticky-sessions],
which is a sticky session. Isoplane sends each request to whichever instance is
free and keeps no sticky sessions, so a connection's requests can reach
different instances. A Redis backplane does not change that. It forwards
messages between servers, and each connection still lives on one.

Statically rendered Blazor pages work.

Typically used for live updates, chat, notifications and interactive Blazor
components.

There is no workaround yet.

### Connections and background work {#connections-and-background-work}

These are described on other pages and listed here so that this page is
complete.

- **WebSockets** are unavailable, because requests arrive as HTTP/1.1 without
  connection upgrades. See
  [Incoming requests](dotnet-runtime-differences.md#incoming-requests).
- **gRPC over HTTP/2** is unavailable, because requests arrive as HTTP/1.1.
  gRPC-Web works, which is a moderate change: it has to be enabled in the
  service and in its clients.
- **A [`BackgroundService`][BackgroundService] that keeps running**, a
  scheduled job or a queue consumer cannot rely on continuous execution. Job
  schedulers and message consumers such as Hangfire, Quartz.NET and
  MassTransit rely on it.
  Background callbacks have a bounded allowance, and an idle instance that
  holds background work is discarded five minutes after the last active
  request finishes, unless another request arrives. A
  `BackgroundService` still running once the host has started is accepted, as
  a timer is, and stops without being cancelled when its instance is
  discarded. Doing that work
  while a request is handled is a large change. See
  [Background work](dotnet-runtime-differences.md#background-work)

## `Microsoft.Extensions.Configuration`

### Configuration providers that load from a remote service {#remote-configuration}

An implementation of `IConfigurationProvider` loads through a synchronous
method, [`IConfigurationProvider.Load`][IConfigurationProvider.Load], so one
that calls a service has to block while the application starts. The application
then refuses to start, with
`unhandled System.PlatformNotSupportedException: Arg_PlatformNotSupported`.

```csharp
builder.Configuration.AddAzureKeyVault(
    new Uri("https://contoso.vault.azure.net/"), new DefaultAzureCredential());
```

The Azure providers registered with [`AddAzureKeyVault`][AddAzureKeyVault]
and [`AddAzureAppConfiguration`][AddAzureAppConfiguration], and the AWS
provider registered with `AddSystemsManager` from the
[`Amazon.Extensions.Configuration.SystemsManager`][nuget-aws-ssm] package,
all load this way.

Typically used for taking secrets and settings from a vault.

#### Workaround: fetch fixed values before building the application, and read changing ones where they are used

`Program.cs` may await before it builds the application, so fetch the values
asynchronously there and add them to the configuration with
[`AddInMemoryCollection`][AddInMemoryCollection]:

```csharp
var builder = IsoplaneHosting.CreateBuilder(args);

// FetchSettingsAsync is yours: an asynchronous call that returns names and
// values, and disposes the client it used before it returns.
Dictionary<string, string?> settings = await FetchSettingsAsync(builder.Configuration);
builder.Configuration.AddInMemoryCollection(settings);

var app = builder.Build();
```

This runs during [initialization](application-lifecycle.md#initialization),
once for each deployment, and every instance starts with the values it
fetched. Two things follow:

- **The values stay fixed until the application is deployed again.** A value
  changed in the store afterwards does not reach the application, not even in
  a new instance. Read a value that has to follow the store, such as a feature
  flag or a rotated credential, from the service where it is used, with its
  asynchronous client, and keep it in memory only for as long as it may be
  stale.
- **Close what the fetch opened.** Initialization refuses to finish while a
  connection is still open, and an `HttpClient` or a service's SDK client keeps
  its connections open for reuse. Dispose the client before
  `FetchSettingsAsync` returns.

For local development,
[User Secrets](../how-to/local-settings.md#keep-credentials-in-user-secrets)
need none of this.

Size of the change: moderate.

## `Microsoft.Extensions.Logging.Console`

### The console logger outside ASP.NET Core, or with another container {#console-logging}

In an application written without ASP.NET Core, adding the console logger to a
logger factory throws
`PlatformNotSupportedException: Arg_PlatformNotSupported`, with
[`Thread.Start`][Thread.Start] near the top of the stack: the console logger
starts a thread to write the entries it queues.

```csharp
using var loggers = LoggerFactory.Create(logging => logging.AddConsole());
```

In an ASP.NET Core application, Isoplane's ASP.NET Core adapter library,
`Isoplane.AspNetCore`, replaces the console logger, so
[`AddConsole`][AddConsole], [`AddJsonConsole`][AddJsonConsole],
[`AddSimpleConsole`][AddSimpleConsole] and
[`AddSystemdConsole`][AddSystemdConsole] work unchanged, with their formatter
options. The exception is a dependency injection container set with
`UseServiceProviderFactory`, such as Autofac's: it copies the registrations
before Isoplane can replace the console logger, which then fails as above.

#### Workaround: log through Isoplane's provider

Add [`IsoplaneLoggerProvider`][IsoplaneLoggerProvider] in place of the console
logger. Its messages reach the same terminal, with their level and category.

```csharp
using var loggers = LoggerFactory.Create(logging => logging.AddProvider(new IsoplaneLoggerProvider()));
```

`Console.WriteLine` also writes to the application's log.

Size of the change: small.

## Entity Framework Core

Entity Framework Core works through its asynchronous methods.

### Lazy loading {#lazy-loading}

With [`UseLazyLoadingProxies`][UseLazyLoadingProxies], from the
[`Microsoft.EntityFrameworkCore.Proxies`][nuget-ef-proxies] package, or an
injected [`ILazyLoader`][ILazyLoader], reading a navigation property runs a
query synchronously and throws
[`PlatformNotSupportedException`][PlatformNotSupportedException]. A navigation
whose target the context already tracks is read from memory, so the same line
can work for one row and fail for another.

```csharp
var order = await db.Orders.FirstAsync(order => order.Id == id);
return order.Customer.Name;   // reading Customer runs a query synchronously: throws
```

Typically used for loading related entities when they are first touched.

#### Workaround: load related data explicitly

Load what the request needs as part of the query, with
[`Include`][Include]:

```csharp
var order = await db.Orders.Include(order => order.Customer).FirstAsync(order => order.Id == id);
```

Or load a navigation explicitly before reading it, with
[`LoadAsync`][ReferenceEntry.LoadAsync]:
`await db.Entry(order).Reference(order => order.Customer).LoadAsync();`.
This is the lasting approach: a property read can only load by
[blocking](dotnet-runtime-differences.md#blocking-waits).

Size of the change: moderate. Every place that relies on a navigation
being loaded on demand has to say what it needs.

### `TransactionScope` {#transaction-scope}

A [`TransactionScope`][TransactionScope], from `System.Transactions`, commits
when it is disposed, and it
can only be disposed synchronously. With work done on a connection inside the
scope, disposing it throws
[`TransactionInDoubtException`][TransactionInDoubtException].

```csharp
using (var scope = new TransactionScope(TransactionScopeAsyncFlowOption.Enabled))
{
    db.Orders.Add(order);
    await db.SaveChangesAsync();
    scope.Complete();
}   // leaving the block commits synchronously: throws
```

Typically used for a transaction around several operations, without passing a
transaction object between them.

#### Workaround: use the database's own transaction

Use the database's own transaction, from
[`Database.BeginTransactionAsync`][BeginTransactionAsync] on the context,
and commit it with its [`CommitAsync`][CommitAsync].

```csharp
await using var transaction = await db.Database.BeginTransactionAsync();
db.Orders.Add(order);
await db.SaveChangesAsync();
await transaction.CommitAsync();
```

Size of the change: small for one context. Moderate when the scope
spans several contexts or connections, which then have to share one
transaction explicitly.

## Database drivers

### SQL Server {#sql-server}

[`Microsoft.Data.SqlClient`][nuget-sqlclient] does not run on Isoplane yet.
Creating a [`SqlConnection`][SqlConnection] throws
`PlatformNotSupportedException: Microsoft.Data.SqlClient is not supported on this platform.`
Everything built on the driver fails with it, including
[`Microsoft.EntityFrameworkCore.SqlServer`][nuget-ef-sqlserver] and its
[`UseSqlServer`][UseSqlServer].

The driver is not short of anything native here. Its fully managed
implementation would load in the sandbox, but it blocks. It signs in
synchronously inside [`SqlConnection.OpenAsync`][DbConnection.OpenAsync], and
it has no asynchronous commit, rollback or close of its own, so those fall back to their
synchronous forms.

Typically used for SQL Server and Azure SQL.

There is no workaround yet.

### SQLite {#sqlite}

Isoplane's runtime includes SQLite, but a database can only be held in memory,
and that memory belongs to one instance. What a request writes is seen only by
requests that reach the same instance, and it is gone when the instance is
discarded. So SQLite suits read-only reference data loaded into each instance,
and scratch data within a request, but not a database the application writes
to and reads back.

Against an in-memory database,
[`Microsoft.Data.Sqlite`][Microsoft.Data.Sqlite] and
[`Microsoft.EntityFrameworkCore.Sqlite`][nuget-ef-sqlite] with its
[`UseSqlite`][UseSqlite] run unchanged, including the functions, aggregates and
collations an application registers, and the ones Entity Framework Core
registers for decimals. A database kept in a file is refused: opening one throws
[`SqliteException`][SqliteException] with
`SQLite Error 14: 'file-backed databases are not available, ...'`. No storage
outlasts an instance, so the file's data would vanish with it, and Isoplane
refuses the file rather than lose the data without telling you.

Typically used for local development databases, tests and small applications.

#### Workaround for reference data: fill an in-memory database as each instance starts

An in-memory database lasts while a connection to it is open, and belongs to
one instance. Each instance has its own, which ends when Isoplane
[discards the instance](application-lifecycle.md#between-requests).

- `Data Source=:memory:` is private to its connection. Keep that connection open
  for as long as you need the data, and hand the open connection to
  [`UseSqlite`][UseSqlite].
- `Data Source=name;Mode=Memory;Cache=Shared` is shared by every connection
  that names it, and lasts while at least one of them is open.

To load reference data, do it when an instance starts, in
[`Activation.Activated`][Activation.Activated], and keep one connection open.
Create that connection there too, not while the application starts:

```csharp
const string Reference = "Data Source=reference;Mode=Memory;Cache=Shared";
SqliteConnection? keepAlive = null;

Activation.Activated += async () =>
{
    keepAlive = new SqliteConnection(Reference);
    await keepAlive.OpenAsync();
    // Create the tables and insert the data here.
};

// Requests open their own connections to the same database.
builder.Services.AddDbContext<CatalogContext>(options => options.UseSqlite(Reference));
```

`Activated` runs once for each instance, so each instance loads the data once.
`Microsoft.Data.Sqlite` keeps a timer that tidies its connection pool, and
like any timer it [stays with the instance](dotnet-runtime-differences.md#background-work)
between requests.

Finish or roll back a transaction before another connection uses the same
tables. A command that finds them locked fails with
`PlatformNotSupportedException` instead of waiting, because
`Microsoft.Data.Sqlite` retries after [`Thread.Sleep`][Thread.Sleep]. An
instance handles several requests at once, taking turns whenever one awaits, so
a transaction that stays open across an `await` can make another request's
command on the same tables fail. Keep transactions short, and await no other
work, such as an outbound call, while one is open.

Extension loading, including SpatiaLite, is not available. The runtime's SQLite
serves the `SQLitePCLRaw` provider that `Microsoft.Data.Sqlite` uses by default,
`e_sqlite3`, from `SQLitePCLRaw` 2.1 or 3.0.

A database the application writes to has no workaround on SQLite: data that
must be shared between requests or outlast an instance belongs in a database
server, such as PostgreSQL; see
[Connect PostgreSQL](../how-to/connect-postgresql.md).

Size of the change: small for reference data loaded as each instance starts.

### MongoDB {#mongodb}

[`MongoDB.Driver`][MongoDB.Driver] starts a thread for each server it
monitors, so in an ordinary deployment, such as a replica set, creating a
`MongoClient` throws
[`PlatformNotSupportedException`][PlatformNotSupportedException] from
[`Thread.Start`][Thread.Start].

```csharp
var client = new MongoClient(connectionString);   // throws
var orders = client.GetDatabase("shop").GetCollection<Order>("orders");
await orders.InsertOneAsync(order);
```

Typically used for MongoDB and compatible document databases.

#### Workaround: connect in load-balanced mode, where the deployment allows it

A load-balanced deployment works, using only the driver's public settings. In
that arrangement the application connects through a load balancer to MongoDB
routers (`mongos`) of a sharded cluster, and the driver monitors no servers, so
it starts no threads. Connect with `loadBalanced=true` and switch off the
connection pool's background maintenance:

```csharp
var settings = MongoClientSettings.FromConnectionString(
    "mongodb://mongodb-lb.internal:27017/?loadBalanced=true");
settings.ClusterConfigurator = cluster =>
    cluster.ConfigureConnectionPool(pool => pool.With(maintenanceInterval: Timeout.InfiniteTimeSpan));
var client = new MongoClient(settings);
```

Few deployments can offer this. The routers have to be started with a
dedicated load-balancer port, behind a load balancer that sends the PROXY
protocol. In 2024 a MongoDB product manager described that arrangement as
[experimental for self-managed deployments][mongodb-load-balancer], not an
officially supported configuration, and Atlas does not offer it for sharded
clusters.

Size of the change: small in code, but for most applications it also means a
different deployment, one MongoDB does not support for production.

## Redis

### StackExchange.Redis 2.x, and the packages that bring it in {#redis}

Version 2.x of [`StackExchange.Redis`][StackExchange.Redis] starts threads for
its connections, so connecting throws
[`PlatformNotSupportedException`][PlatformNotSupportedException] from
[`Thread.Start`][Thread.Start]. Everything built on 2.x fails the same way,
and Microsoft's Redis packages all bring it in:

- `Microsoft.Extensions.Caching.StackExchangeRedis`, for the distributed cache
  registered with [`AddStackExchangeRedisCache`][AddStackExchangeRedisCache],
  which is also what [`HybridCache`][HybridCache] uses as a Redis second level.
- `Microsoft.AspNetCore.OutputCaching.StackExchangeRedis`, for the output
  cache registered with
  [`AddStackExchangeRedisOutputCache`][AddStackExchangeRedisOutputCache].
- `Microsoft.AspNetCore.DataProtection.StackExchangeRedis`, for
  `PersistKeysToStackExchangeRedis`.
- `Microsoft.AspNetCore.SignalR.StackExchangeRedis`, for SignalR's Redis
  backplane.

Other packages do too, such as [`RedisRateLimiting`][nuget-redis-rate-limiting].
The distributed cache, for example, fails when it is first used:

```csharp
builder.Services.AddStackExchangeRedisCache(options =>
    options.Configuration = builder.Configuration.GetConnectionString("Redis"));   // fails when the cache is first used
```

Typically used for a cache shared between instances, session state, and Data
Protection keys.

#### Workaround: reference the stable 3.x client, and call it asynchronously

The current stable line, 3.x, [rewrote its I/O][se-redis-3] and starts no
threads. Add a direct reference to it, which replaces the 2.x that the
packages above would bring in, and leave the rest of the application as it
is:

```xml
<PackageReference Include="StackExchange.Redis" Version="3.4.20" />
```

[`IDistributedCache`][IDistributedCache] through `AddStackExchangeRedisCache`
then works, and so does the client used directly:

```csharp
await using var redis = await ConnectionMultiplexer.ConnectAsync(connectionString);
var db = redis.GetDatabase();
await db.StringSetAsync("greeting", "kia ora", TimeSpan.FromSeconds(30));
var greeting = (string?)await db.StringGetAsync("greeting");
```

[`HybridCache`][HybridCache] with Redis as its second level, and session state
[loaded with `UseLoadedSession`](#session), use the same cache and work the
same way.

What to know before relying on it:

- **Use the asynchronous methods only.** `ConnectionMultiplexer.Connect`, the
  synchronous `IDistributedCache` methods and synchronous commands such as
  `db.StringGet(...)` throw `PlatformNotSupportedException`.
- **A kept client retains instance resources.** Its outstanding read transfers
  to background accounting when the request completes, without keeping the
  request open. Later requests can reuse the client. The connection counts
  against the instance's connection limit until closed; reconnect if its peer,
  network or instance lifecycle closes it.
- **A shared client can time out after the instance's background allowance
  runs out.** Its maintenance and completions wait until background execution
  resumes, so a request awaiting it can fail with a Redis timeout. See
  [background limits](resource-limits.md#background-allowance).
- **Connect inside a request, not during initialization.** A connected client
  holds a socket, and initialization refuses an open socket.
- **The Redis output cache works with the same client override.**
- **The override does not rescue every package above.** The Data Protection
  store still fails, because Data Protection reads its keys synchronously; see
  [Data Protection key stores](#data-protection). SignalR is not supported
  with or without the backplane; see [SignalR](#signalr).

Size of the change: small.

## Observability

### OpenTelemetry exporters that send over the network {#opentelemetry}

The OpenTelemetry SDK and its instrumentation, such as
`AddAspNetCoreInstrumentation`, work: spans and measurements are recorded as
they are anywhere else. Sending them is what fails. An exporter's `Export`
method is synchronous by definition, so an exporter that sends over the network
has to [wait synchronously](dotnet-runtime-differences.md#blocking-waits) for each send.
The common one, the OTLP exporter, delivers nothing to its collector, for
traces, metrics and logs, over HTTP and over gRPC alike.

```csharp
builder.Services.AddOpenTelemetry()
    .WithTracing(tracing => tracing
        .AddAspNetCoreInstrumentation()
        .AddOtlpExporter());   // nothing arrives at the collector
```

Typically used for sending traces, metrics and logs to a collector.

#### Workaround: send traces with Isoplane.OpenTelemetry

`Isoplane.OpenTelemetry` exports traces with the SDK's own OTLP exporter, and
sends each request's spans after its response. Add the package, and call
`AddIsoplaneOtlpExporter` in place of `AddOtlpExporter`:

```bash
dotnet add package Isoplane.OpenTelemetry --version 0.1.0-preview.5
```

```csharp
using Isoplane.OpenTelemetry;
using OpenTelemetry.Trace;

builder.Services.AddOpenTelemetry()
    .WithTracing(tracing => tracing
        .AddAspNetCoreInstrumentation()
        .AddIsoplaneOtlpExporter());
```

It reads the same [`OTEL_EXPORTER_OTLP_*` settings][otlp-exporter-settings]
from the application's configuration as `AddOtlpExporter`, such as
`OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_EXPORTER_OTLP_PROTOCOL`, and takes the
same options in code. As with `AddOtlpExporter`, the default is gRPC to
`http://localhost:4317`; set `OTEL_EXPORTER_OTLP_PROTOCOL` to `http/protobuf`
for an endpoint that takes OTLP over HTTP.

The spans of a request are held in the instance's memory until the request's
own span ends, then encoded and sent while the client already has the response.
Work retained after response completion uses the instance's
[background allowance](resource-limits.md#background-allowance).
A request that produces many spans can send the ones that have ended sooner,
and learn whether the endpoint accepted them:

```csharp
app.MapPost("/import", async (TraceExport traces) =>
{
    // ... work that ends many spans ...
    await traces.FlushAsync();
});
```

What it does not do:

- **Keep spans the instance loses.** Spans not yet sent when an instance is
  stopped are lost.
- **Retry.** An export the endpoint refuses, or that fails to send, is logged
  and dropped.
- **Metrics and logs.** It covers traces only; the SDK's metrics and logs
  exporters still deliver nothing.

Size of the change: small.

## Model Context Protocol

### An MCP server that tracks sessions {#mcp-sessions}

[`ModelContextProtocol.AspNetCore`][nuget-mcp] serves the Model Context
Protocol over Streamable HTTP. In its [stateless mode][mcp-stateless] it works.
With session tracking switched on, by setting `Stateless` to `false` or
`SessionMode` to anything but `Stateless`, the server keeps each session in the
memory of the instance that began it. Nothing fails when the application
starts, or for a client whose requests happen to reach that instance. A
request that reaches another instance, or that arrives after the instance is
discarded, is answered with 404 and the JSON-RPC error `Session not found`
(`-32001`), so the failures are intermittent.

```csharp
builder.Services
    .AddMcpServer()
    .WithHttpTransport(options => options.Stateless = false)   // starts, then fails intermittently
    .WithTools<HandbookTools>();
```

Typically used for servers that start exchanges of their own with a client:
sampling, elicitation and unsolicited notifications.

#### Workaround: use stateless mode

Set [`Stateless`][mcp-http-options] to `true` on the HTTP transport options.
Recent versions default to it; setting it keeps it if the default moves. Every
request then stands alone: the server answers calls, and cannot start an
exchange of its own. The
[`isoplane-mcpserver`](../reference/project-templates.md#isoplane-mcpserver)
template is a server set up this way.

Size of the change: small for a server that only answers tool calls. A
server that depends on sampling or elicitation has no workaround.

## Check a package yourself {#check-a-package}

Build with
[`IsoplaneAnalyzeDependencies`](../reference/project-settings.md#isoplaneanalyzedependencies)
set. The build then names every package assembly that contains a call that
starts a thread or waits synchronously. Treat the result as a list of things
to try, not a verdict: it flags an assembly for a call anywhere inside it, and
that includes Npgsql and Entity Framework Core, whose asynchronous paths work.

[AddFixedWindowLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.ratelimiting.ratelimiteroptionsextensions.addfixedwindowlimiter
[AddSlidingWindowLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.ratelimiting.ratelimiteroptionsextensions.addslidingwindowlimiter
[AddTokenBucketLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.ratelimiting.ratelimiteroptionsextensions.addtokenbucketlimiter
[AddConcurrencyLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.ratelimiting.ratelimiteroptionsextensions.addconcurrencylimiter
[RateLimiter.AttemptAcquire]: https://learn.microsoft.com/dotnet/api/system.threading.ratelimiting.ratelimiter.attemptacquire
[RateLimiter.AcquireAsync]: https://learn.microsoft.com/dotnet/api/system.threading.ratelimiting.ratelimiter.acquireasync
[nuget-redis-rate-limiting]: https://www.nuget.org/packages/RedisRateLimiting
[signalr-sticky-sessions]: https://learn.microsoft.com/aspnet/core/signalr/scale#sticky-sessions
[Activation.Activated]: ../reference/sdk/Isoplane.Sdk/Isoplane/Activation/Activated.md
[AddAzureAppConfiguration]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.configuration.azureappconfigurationextensions.addazureappconfiguration
[AddAzureKeyVault]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.configuration.azurekeyvaultconfigurationextensions.addazurekeyvault
[AddConsole]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.consoleloggerextensions.addconsole
[AddDistributedMemoryCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.memorycacheservicecollectionextensions.adddistributedmemorycache
[AddInMemoryCollection]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.configuration.memoryconfigurationbuilderextensions.addinmemorycollection
[AddJsonConsole]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.consoleloggerextensions.addjsonconsole
[AddJsonOptions]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.mvccoremvcbuilderextensions.addjsonoptions
[AddNewtonsoftJson]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.newtonsoftjsonmvcbuilderextensions.addnewtonsoftjson
[AddSession]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.sessionservicecollectionextensions.addsession
[AddSimpleConsole]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.consoleloggerextensions.addsimpleconsole
[AddSystemdConsole]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.consoleloggerextensions.addsystemdconsole
[AddSessionStateTempDataProvider]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.mvcviewfeaturesmvcbuilderextensions.addsessionstatetempdataprovider
[AddStackExchangeRedisCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.stackexchangerediscacheservicecollectionextensions.addstackexchangerediscache
[AddStackExchangeRedisOutputCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.stackexchangeredisoutputcacheservicecollectionextensions.addstackexchangeredisoutputcache
[AddXmlSerializerFormatters]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.mvcxmlmvcbuilderextensions.addxmlserializerformatters
[AsFileSection]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.webutilities.multipartsectionconverterextensions.asfilesection
[BackgroundService]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.hosting.backgroundservice
[BeginTransactionAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.infrastructure.databasefacade.begintransactionasync
[CommitAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.storage.idbcontexttransaction.commitasync
[ConfigureHttpJsonOptions]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.httpjsonserviceextensions.configurehttpjsonoptions
[Controller.TempData]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.controller.tempdata
[CryptographicException]: https://learn.microsoft.com/dotnet/api/system.security.cryptography.cryptographicexception
[CryptoStream]: https://learn.microsoft.com/dotnet/api/system.security.cryptography.cryptostream
[DbConnection.OpenAsync]: https://learn.microsoft.com/dotnet/api/system.data.common.dbconnection.openasync
[DbContext.SaveChanges]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbcontext.savechanges
[DbContext.SaveChangesAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbcontext.savechangesasync
[DllNotFoundException]: https://learn.microsoft.com/dotnet/api/system.dllnotfoundexception
[EnableBuffering]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequestrewindextensions.enablebuffering
[FixedWindowRateLimiter]: https://learn.microsoft.com/dotnet/api/system.threading.ratelimiting.fixedwindowratelimiter
[fluentvalidation-async]: https://docs.fluentvalidation.net/en/latest/async.html
[FormOptions]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions
[FormOptions.MemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions.memorybufferthreshold
[FormOptions.MultipartBodyLengthLimit]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions.multipartbodylengthlimit
[HttpContext.Session]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httpcontext.session
[HttpRequest.ReadFormAsync]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequest.readformasync
[HybridCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.caching.hybrid.hybridcache
[IConfigurationProvider.Load]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.configuration.iconfigurationprovider.load
[IDataProtectionBuilder]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.idataprotectionbuilder
[IDistributedCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.caching.distributed.idistributedcache
[IFormFile]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.iformfile
[ILazyLoader]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.infrastructure.ilazyloader
[Include]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.entityframeworkqueryableextensions.include
[InputFormatterMemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.mvcnewtonsoftjsonoptions.inputformattermemorybufferthreshold
[IsoplaneLoggerProvider]: ../reference/sdk/Isoplane.Sdk/Isoplane/IsoplaneLoggerProvider.md
[IssuerSigningKeyResolver]: https://learn.microsoft.com/dotnet/api/microsoft.identitymodel.tokens.tokenvalidationparameters.issuersigningkeyresolver
[IStringLocalizer]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.localization.istringlocalizer
[IValidatableObject.Validate]: https://learn.microsoft.com/dotnet/api/system.componentmodel.dataannotations.ivalidatableobject.validate
[IXmlRepository]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.repositories.ixmlrepository
[JwtBearerOptions.Authority]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.authentication.jwtbearer.jwtbeareroptions.authority
[JwtBearerOptions.MetadataAddress]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.authentication.jwtbearer.jwtbeareroptions.metadataaddress
[mcp-http-options]: https://csharp.sdk.modelcontextprotocol.io/v2/api/ModelContextProtocol.AspNetCore.HttpServerTransportOptions.html
[mcp-stateless]: https://csharp.sdk.modelcontextprotocol.io/v2/concepts/stateless/stateless.html
[Microsoft.Data.Sqlite]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlite
[MongoDB.Driver]: https://www.mongodb.com/docs/drivers/csharp/current/
[mongodb-load-balancer]: https://dev.to/alexbevi/mongodb-and-load-balancer-support-1p28
[MultipartReader]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.webutilities.multipartreader
[NetworkStream]: https://learn.microsoft.com/dotnet/api/system.net.sockets.networkstream
[nuget-aws-ssm]: https://www.nuget.org/packages/Amazon.Extensions.Configuration.SystemsManager
[nuget-dp-ef]: https://www.nuget.org/packages/Microsoft.AspNetCore.DataProtection.EntityFrameworkCore
[nuget-ef-proxies]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.Proxies
[nuget-ef-sqlite]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.Sqlite
[nuget-ef-sqlserver]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.SqlServer
[nuget-mcp]: https://www.nuget.org/packages/ModelContextProtocol.AspNetCore
[nuget-newtonsoft]: https://www.nuget.org/packages/Microsoft.AspNetCore.Mvc.NewtonsoftJson
[nuget-sqlclient]: https://www.nuget.org/packages/Microsoft.Data.SqlClient
[OutputFormatterMemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.mvcnewtonsoftjsonoptions.outputformattermemorybufferthreshold
[PersistKeysToDbContext]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.entityframeworkcoredataprotectionextensions.persistkeystodbcontext
[PersistKeysToFileSystem]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.dataprotectionbuilderextensions.persistkeystofilesystem
[PlatformNotSupportedException]: https://learn.microsoft.com/dotnet/api/system.platformnotsupportedexception
[rdg]: https://learn.microsoft.com/aspnet/core/fundamentals/aot/request-delegate-generator/rdg
[ReferenceEntry.LoadAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.changetracking.referenceentry.loadasync
[se-redis-3]: https://github.com/StackExchange/StackExchange.Redis/releases/tag/3.0.0
[SemaphoreSlim.Wait]: https://learn.microsoft.com/dotnet/api/system.threading.semaphoreslim.wait
[Socket]: https://learn.microsoft.com/dotnet/api/system.net.sockets.socket
[SqlConnection]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlclient.sqlconnection
[SqliteException]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlite.sqliteexception
[StackExchange.Redis]: https://stackexchange.github.io/StackExchange.Redis/
[stj-source-generation]: https://learn.microsoft.com/dotnet/standard/serialization/system-text-json/source-generation
[System.Text.Json]: https://learn.microsoft.com/dotnet/api/system.text.json
[Task]: https://learn.microsoft.com/dotnet/api/system.threading.tasks.task
[Task.Result]: https://learn.microsoft.com/dotnet/api/system.threading.tasks.task-1.result
[Thread.Sleep]: https://learn.microsoft.com/dotnet/api/system.threading.thread.sleep
[Thread.Start]: https://learn.microsoft.com/dotnet/api/system.threading.thread.start
[TokenBucketRateLimiter]: https://learn.microsoft.com/dotnet/api/system.threading.ratelimiting.tokenbucketratelimiter
[TokenValidationParameters]: https://learn.microsoft.com/dotnet/api/microsoft.identitymodel.tokens.tokenvalidationparameters
[TransactionInDoubtException]: https://learn.microsoft.com/dotnet/api/system.transactions.transactionindoubtexception
[TransactionScope]: https://learn.microsoft.com/dotnet/api/system.transactions.transactionscope
[UseLazyLoadingProxies]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.proxiesextensions.uselazyloadingproxies
[UseLoadedSession]: ../reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/SessionLoading/UseLoadedSession.md
[UseRateLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.builder.ratelimiterapplicationbuilderextensions.useratelimiter
[UseSession]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.builder.sessionmiddlewareextensions.usesession
[UseSqlite]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.sqlitedbcontextoptionsbuilderextensions.usesqlite
[UseSqlServer]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.sqlserverdbcontextoptionsextensions.usesqlserver
[ValidationAttribute.IsValid]: https://learn.microsoft.com/dotnet/api/system.componentmodel.dataannotations.validationattribute.isvalid
[XDocument.LoadAsync]: https://learn.microsoft.com/dotnet/api/system.xml.linq.xdocument.loadasync
[XDocument.SaveAsync]: https://learn.microsoft.com/dotnet/api/system.xml.linq.xdocument.saveasync
[XmlReader]: https://learn.microsoft.com/dotnet/api/system.xml.xmlreader
[XmlReaderSettings.Async]: https://learn.microsoft.com/dotnet/api/system.xml.xmlreadersettings.async
[XmlSerializer]: https://learn.microsoft.com/dotnet/api/system.xml.serialization.xmlserializer
[XmlWriter]: https://learn.microsoft.com/dotnet/api/system.xml.xmlwriter
[otlp-exporter-settings]: https://opentelemetry.io/docs/specs/otel/protocol/exporter/
