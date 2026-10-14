---
title: Library compatibility
---

Most NuGet packages run on Isoplane unchanged. The ones that do not fail for a
handful of reasons, all of which follow from
[how .NET differs on Isoplane](dotnet-runtime-differences.md): something waits
synchronously, starts a thread or a timer, writes a file, or loads a native
library. That page also explains
[what those limits are the price of](dotnet-runtime-differences.md#why-the-sandbox-works-this-way).

This page lists the specific incompatibilities we know of, by package: APIs,
packages and framework features that fail even though your own code is
asynchronous.

It does not list synchronous methods whose whole remedy is to call their
asynchronous counterpart, such as Entity Framework Core's
[`SaveChanges`][DbContext.SaveChanges], which becomes
[`SaveChangesAsync`][DbContext.SaveChangesAsync], or a task read with
[`Task.Result`][Task.Result], which is awaited instead. Most packages have
some, and one rule covers them all:
[Waiting is asynchronous](dotnet-runtime-differences.md#waiting-is-asynchronous).

Each entry says what fails and how to work around it today, with the size of
that change. Where there is more than one way through, the entry says which we
recommend and gives the other as an alternative. This list is meant to shrink:
where a later release is expected to remove a problem, or to make its
workaround smaller, the entry says so under "Looking ahead". [Packages that work](#works) and
[packages not checked yet](#not-checked) are at the end.

Everything here was run in the sandbox with SDK 0.1.0, unless its
entry says otherwise.

## At a glance {#summary}

| Package | API or feature | What to do |
| --- | --- | --- |
| ASP.NET Core | [Session over a distributed cache, and TempData kept in it](#session) | Small change: add one middleware |
| ASP.NET Core | [File uploads over 64 KB](#uploads) | Moderate change: read the upload as a stream. Or hold small uploads in memory |
| ASP.NET Core | [`Request.EnableBuffering()` over 30 KB](#enable-buffering) | Moderate change: read the body once, as a stream. Or buffer up to a limit |
| ASP.NET Core | [Minimal API handlers that use source-generated code](#generated-code-in-handlers) | Small change: register the JSON context and return objects. Or switch one generator off |
| ASP.NET Core | [Data Protection key stores](#data-protection) | Small change: use the replacement Isoplane provides |
| ASP.NET Core | [XML formatters over 30 KB](#xml-formatters) | Moderate change: use the asynchronous XML APIs. Or buffer up to a limit |
| ASP.NET Core | [Rate limiting middleware](#rate-limiting) | Large change: limit requests elsewhere. Or a per-instance limiter topped up on each request |
| ASP.NET Core | [Console logging: `AddConsole`, `AddJsonConsole` and `ClearProviders`](#console-logging) | Small change: delete the calls. A later release should not need it |
| ASP.NET Core | [Validation attributes, localizers, token callbacks and service factories that do I/O](#synchronous-extension-points) | Moderate change: move the I/O elsewhere |
| ASP.NET Core | [WebSockets, SignalR's WebSocket transport, Blazor Server](#connections-and-background-work) | Unsupported |
| ASP.NET Core | [gRPC over HTTP/2](#connections-and-background-work) | Moderate change: use gRPC-Web |
| ASP.NET Core | [`BackgroundService` and other work between requests](#connections-and-background-work) | Large change: do the work during a request |
| Newtonsoft.Json for MVC | [Bodies over 30 KB](#newtonsoft-json) | Moderate to large change: move to `System.Text.Json`. Or keep Newtonsoft.Json and raise two limits |
| Microsoft.Extensions.Configuration | [`IConfigurationProvider` implementations that load from a remote service, such as Azure Key Vault](#remote-configuration) | Moderate change: fetch them before `builder.Build()` |
| Entity Framework Core | [Lazy loading](#lazy-loading) | Moderate change: load with `Include` |
| Entity Framework Core | [`TransactionScope`](#transaction-scope) | Small change: use a database transaction |
| SqlClient | [SQL Server, including its Entity Framework Core provider](#sql-server) | Not supported yet: waits on work under way in the driver. PostgreSQL works today |
| Sqlite | [SQLite, including its Entity Framework Core provider](#sqlite) | Not supported yet: needs SQLite in Isoplane's runtime. PostgreSQL works today |
| MongoDB.Driver | [MongoDB](#mongodb) | Not supported yet for most deployments: waits on the driver. A load-balanced deployment works with two settings |
| StackExchange.Redis | [Redis, including the Redis distributed cache](#redis) | Small change: reference the client's 4.0 prerelease and call it asynchronously |
| OpenTelemetry | [OpenTelemetry SDK](#opentelemetry) | Large change, traces only: collect spans without the SDK and send them before the response completes |
| ModelContextProtocol | [An MCP server that tracks sessions](#mcp-sessions) | Small change: use stateless mode |

The sections below give each package's full name.

<details>
<summary>How the changes are sized</summary>

- **Small** is a one-for-one replacement: the asynchronous form of the same
  call, a setting, or a method Isoplane provides.
- **Moderate** is some restructuring, or a few lines of your own.
- **Large** is a different library or a different approach.

</details>

## Recognize a failure {#symptoms}

Exception messages from the runtime are short identifiers, not sentences, so
read the stack trace to see which call failed.

| What you see | What it means |
| --- | --- |
| `PlatformNotSupportedException: Arg_PlatformNotSupported`, with `Task.InternalWait` (internal to [`Task`][Task]), [`SemaphoreSlim.Wait`][SemaphoreSlim.Wait], [`Thread.Sleep`][Thread.Sleep], [`Socket`][Socket] or [`NetworkStream`][NetworkStream] near the top of the stack | Something waited synchronously. If the call is in your own code, see [Waiting is asynchronous](dotnet-runtime-differences.md#waiting-is-asynchronous). If it is inside a library, find the library in the table above. |
| `PlatformNotSupportedException: net_wasi_sync_name_resolution` | The same, for a connection that was opened synchronously. |
| `PlatformNotSupportedException: Arg_PlatformNotSupported`, with [`Thread.Start`][Thread.Start] near the top of the stack | A library started a thread. See [MongoDB](#mongodb) and [Redis](#redis). |
| `DirectoryNotFoundException: /tmp/` | ASP.NET Core tried to move a large request or response to a temporary file. See [uploads](#uploads), [request buffering](#enable-buffering), [Newtonsoft.Json](#newtonsoft-json) and [XML formatters](#xml-formatters). |
| `IOException: Read-only file system` | Something tried to write a file. The [file system is read-only](dotnet-runtime-differences.md#the-machine-underneath). |
| [`DllNotFoundException`][DllNotFoundException], sometimes inside a `TypeInitializationException` | A package called into a native library. See [SQLite](#sqlite). |
| The application refuses to start with `initialization retains a timer` | Something registered at startup started a timer. The lines beneath the message name each timer and the method that runs or awaits it. See [rate limiting](#rate-limiting) and [OpenTelemetry](#opentelemetry). |
| The application refuses to start with `unhandled System.PlatformNotSupportedException` and no stack trace | A call made while the application starts failed. The known ones are [console logging](#console-logging) and [remote configuration](#remote-configuration). |
| The application refuses to start, naming a service that was `Still running once the application had started` | A package registered a background service. See [MCP sessions](#mcp-sessions) and [background work](#connections-and-background-work). |
| A minimal API endpoint answers 200 with an empty body, or the build fails with `CS9144: Cannot intercept method` | A handler names code that another source generator produces. See [generated code in handlers](#generated-code-in-handlers). |
| No error, but session values or TempData are missing on the next request | The session was loaded synchronously. See [session](#session). |

## ASP.NET Core

These ship with the Isoplane SDK as part of ASP.NET Core, so they need no
package reference.

### Session over a distributed cache {#session}

With [`AddSession`][AddSession] and an [`IDistributedCache`][IDistributedCache]
that reaches a database or another service, the first read or write of
[`HttpContext.Session`][HttpContext.Session] in a request loads the session
synchronously. The load fails, ASP.NET Core logs `Session cache read exception`
under the category `Microsoft.AspNetCore.Session.DistributedSession`, and the
request carries on with an empty session. Reads return nothing and writes are
not saved. No exception reaches your code.

```csharp
app.MapPost("/cart/items", (HttpContext context, string item) =>
{
    var cart = context.Session.GetString("cart");          // null: the load failed
    context.Session.SetString("cart", $"{cart},{item}");   // not saved
});
```

[TempData][Controller.TempData] is lost the same way when it is kept in the
session with MVC's
[`AddSessionStateTempDataProvider`][AddSessionStateTempDataProvider].

Typically used for sign-in flows, shopping carts, multi-step forms, and messages
shown after a redirect.

We ran this with a PostgreSQL-backed cache. The Redis cache does not run at
all; see [Redis](#redis).

#### Workaround: load the session in a middleware

Load the session with
[`ISession.LoadAsync`][ISession.LoadAsync] before anything uses it, in a middleware
placed directly after [`UseSession`][UseSession]:

```csharp
app.UseSession();
app.Use(async (context, next) =>
{
    await context.Session.LoadAsync(context.RequestAborted);
    await next();
});
```

Size of the change: small. One middleware, added once.

#### Looking ahead

The workaround should get smaller. Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`,
can provide this middleware, and report an unloaded session as an error in
place of losing data.

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
as a stream: every upload then counts toward the instance's [memory limit](execution-limits.md#memory), and the
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

#### Looking ahead

A later release is expected to let `IFormFile` work as written for uploads that
fit in memory. Reading a large upload as a stream stays the better choice.

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
instance's [memory limit](execution-limits.md#memory), so prefer the single pass where it fits.

Size of the change: small.

#### Looking ahead

A later release is expected to let `EnableBuffering` work as written for bodies
that fit in memory. Reading the body once stays the better choice.

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

Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`,
which every Isoplane ASP.NET Core project already references, provides
replacements. They are described in
[Keep Data Protection keys across builds](dotnet-runtime-differences.md#keep-data-protection-keys-across-builds).
- `UseKeysFromDbContext` takes the place of `PersistKeysToDbContext` and uses
  the same table. It is an extension method on
  [`IDataProtectionBuilder`][IDataProtectionBuilder] in the
  `Isoplane.AspNetCore` namespace, which the adapter library adds to an
  application that references the
  [`Microsoft.AspNetCore.DataProtection.EntityFrameworkCore`][nuget-dp-ef]
  package. It has no reference page yet.
- [`UseConfiguredKeys`](reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/DataProtectionKeys/UseConfiguredKeys.md)
  derives the keys from a secret in the application's settings.
- [`UseKeysFrom`](reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/DataProtectionKeys/UseKeysFrom.md)
  serves any other store.

Size of the change: small for Entity Framework Core, where one call
replaces another. Moderate for another store, where you supply the two
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

The whole body is then held in memory, counted toward the instance's [memory limit](execution-limits.md#memory).

Size of the change: small to moderate.

#### Looking ahead

A later release is expected to make the XML formatters work as written for
bodies that fit in memory.

### Rate limiting middleware {#rate-limiting}

An application that calls [`UseRateLimiter`][UseRateLimiter] refuses to start:
`initialization retains a timer`. This is so with every built-in policy (fixed
window, sliding window, token bucket and concurrency) and with no policy at
all, because the timer is the middleware's own: it ticks every 100 ms to refill
permits and tidy up, and a timer cannot be carried from initialization into an
instance.

```csharp
builder.Services.AddRateLimiter(options =>
    options.AddFixedWindowLimiter("api", limiter =>
    {
        limiter.PermitLimit = 100;
        limiter.Window = TimeSpan.FromMinutes(1);
    }));

var app = builder.Build();
app.UseRateLimiter();   // the application refuses to start
```

The policy above is registered with [`AddRateLimiter`][AddRateLimiter] and
[`AddFixedWindowLimiter`][AddFixedWindowLimiter].

Typically used for limiting how often a client may call an endpoint.

#### Recommended workaround: limit requests outside the application

Count requests in a store that every instance shares, or limit them in front
of the application. A limit kept in an application's memory holds for one
instance only: Isoplane runs as many instances as the traffic needs, and
[memory belongs to one instance](dotnet-runtime-differences.md#instances-come-and-go).

Size of the change: large. Limiting moves out of the application.

#### Alternative workaround: a limiter topped up on each request

.NET's limiters can be used without the middleware. Create a
[`FixedWindowRateLimiter`][FixedWindowRateLimiter] or a
[`TokenBucketRateLimiter`][TokenBucketRateLimiter] with its own timer switched
off, and call [`TryReplenish`][TryReplenish] before asking for a permit. It
works out what to add from the time that has passed, so permits come back
after an idle spell although nothing ran in between.

```csharp
using System.Threading.RateLimiting;

var limiters = new Dictionary<string, FixedWindowRateLimiter>();

app.Use(async (context, next) =>
{
    // One limiter for each caller, identified here by an API key header.
    var client = context.Request.Headers["X-Api-Key"].ToString();
    if (!limiters.TryGetValue(client, out var limiter))
        limiters[client] = limiter = new FixedWindowRateLimiter(new()
        {
            PermitLimit = 100,
            Window = TimeSpan.FromMinutes(1),
            AutoReplenishment = false,   // no timer; each request tops the limiter up
        });

    limiter.TryReplenish();
    using var lease = limiter.AttemptAcquire();
    if (!lease.IsAcquired)
    {
        context.Response.StatusCode = StatusCodes.Status429TooManyRequests;
        return;
    }
    await next(context);
});
```

Know its limits before relying on it:

- It counts for one instance. With several instances, each allows the full
  limit, and the counts are gone whenever an instance is replaced.
- The sliding-window limiter does not work this way: it moves on by one
  segment for each call, so it stays closed after an idle spell.
- Leave queueing off. A queued request would wait for a refill that only the
  next request brings.
- `RequireRateLimiting` and `[EnableRateLimiting]` belong to the middleware and
  have no effect here.
- Nothing removes a caller's limiter once it is idle, so bound the number of
  keys yourself if callers are many.

Size of the change: small to moderate.

#### Looking ahead

Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`, can provide a
middleware that honours the policies an application already registers and
refills them this way, so that `UseRateLimiter` has a one-line replacement.
Counting across instances needs shared state, which no in-memory limiter has.

### Console logging: `AddConsole`, `AddJsonConsole` and `ClearProviders` {#console-logging}

An application that calls [`AddConsole`][AddConsole] or
[`AddJsonConsole`][AddJsonConsole] on `builder.Logging` refuses to start, with
`unhandled System.PlatformNotSupportedException: Arg_PlatformNotSupported` and
no stack trace: the console logger starts a thread.

[`ClearProviders`][ClearProviders] on its own lets the application start, but
removes the provider Isoplane registers, and [`ILogger`][ILogger] output is no
longer shown.

```csharp
builder.Logging.ClearProviders();
builder.Logging.AddConsole();
```

These lines are in many `Program.cs` files, often copied from a template.

#### Workaround: delete the calls

Delete them. Isoplane already sends `ILogger` output to
the terminal that runs the session, and the `Logging` section of
`appsettings.json` filters it as usual. Serilog's
[console sink][serilog-console] also works.

Size of the change: small.

#### Looking ahead

A later release is expected to make these calls
harmless. Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`, can keep
its own provider registered and leave the console provider out.

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
  [`Initialization.Initializing`](reference/sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing.md)
  callback and serve them from memory. What initialization leaves in memory is
  copied into every instance.
- **Signing keys.** Set
  [`JwtBearerOptions.Authority`][JwtBearerOptions.Authority] or
  [`JwtBearerOptions.MetadataAddress`][JwtBearerOptions.MetadataAddress]. The
  handler then fetches and refreshes the keys asynchronously.
- **Services.** Construct the object without connecting, and connect
  asynchronously the first time it is used.

Size of the change: moderate. The I/O moves to a place that can await it.

#### Looking ahead

These interfaces can only do I/O by
[blocking](dotnet-runtime-differences.md#never-blocks), so the I/O has to
move. Where one is common enough, Isoplane can provide a replacement that
makes the change smaller, as it does for
[Data Protection keys](#data-protection).

### Connections and background work {#connections-and-background-work}

These are described on other pages and listed here so that this page is
complete.

- **WebSockets, SignalR's WebSocket transport and Blazor Server
  interactivity** are unavailable, and nothing takes their place today.
  Statically rendered Blazor pages work. See [Network](dotnet-runtime-differences.md#network).
- **gRPC over HTTP/2** is unavailable, because requests arrive as HTTP/1.1.
  gRPC-Web works, which is a moderate change: it has to be enabled in the
  service and in its clients. Whether HTTP/2 can be supported is not known
  yet.
- **A [`BackgroundService`][BackgroundService] that keeps running**, a
  recurring timer or a queue consumer has nothing to run on between requests.
  Doing that work while a request is handled is a large change. See
  [Work belongs to a request](dotnet-runtime-differences.md#work-belongs-to-a-request).

## `Microsoft.AspNetCore.Mvc.NewtonsoftJson`

### Bodies over 30 KB {#newtonsoft-json}

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
instance's [memory limit](execution-limits.md#memory), and a larger one fails as before.

Size of the change: small.

#### Looking ahead

A later release is expected to make the thresholds unnecessary for bodies that
fit in memory.

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

We confirmed this with a provider written for the purpose. The Azure
providers registered with [`AddAzureKeyVault`][AddAzureKeyVault] and
[`AddAzureAppConfiguration`][AddAzureAppConfiguration], and the AWS provider
registered with `AddSystemsManager` from the
[`Amazon.Extensions.Configuration.SystemsManager`][nuget-aws-ssm] package,
load the same way and are expected to fail likewise. They have not been run in
the sandbox.

Typically used for taking secrets and settings from a vault.

#### Workaround: fetch the settings before building the application

`Program.cs` may await before it builds the
application, so fetch the values asynchronously there and add them to the configuration with
[`AddInMemoryCollection`][AddInMemoryCollection]:

```csharp
var builder = IsoplaneHosting.CreateBuilder(args);

// FetchSettingsAsync is yours: an asynchronous call that returns names and values.
Dictionary<string, string?> settings = await FetchSettingsAsync(builder.Configuration);
builder.Configuration.AddInMemoryCollection(settings);

var app = builder.Build();
```

This runs during [initialization](application-lifecycle.md#initialization),
so the values are fetched once and every instance starts with them. For
local development, [User Secrets](local-settings.md#keep-credentials-in-user-secrets)
need none of this.

Size of the change: moderate.

#### Looking ahead

The workaround should get smaller. An adapter library
from Isoplane can provide an asynchronous registration method for each
provider, which would make it one line.

{/* TODO: say how settings and secrets reach a deployed application once that exists; it is expected to make this entry unnecessary for most applications. */}

## Entity Framework Core

Entity Framework Core works through its asynchronous methods. We ran it with
the Npgsql provider; see [Packages that work](#works).

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
[blocking](dotnet-runtime-differences.md#never-blocks).

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

The driver is not short of anything native here: it has a fully managed
implementation, which we ran in the sandbox. That implementation blocks. It
signs in synchronously inside
[`SqlConnection.OpenAsync`][DbConnection.OpenAsync], and it has no asynchronous
commit, rollback or close of its own, so those fall back to their synchronous
forms.

Typically used for SQL Server and Azure SQL.

#### Workaround: none for SQL Server itself, yet

PostgreSQL through Npgsql works today; see
[Connect PostgreSQL](connect-postgresql.md).

Size of the change: large: a different database.

#### Looking ahead

What stands in the way is being worked on in the driver itself, so we expect
this to change.

- The driver's maintainers track the blocking sign-in as a known problem
  ([dotnet/SqlClient#979][sqlclient-979]), under their work to
  [improve the asynchronous paths of its connection APIs][sqlclient-3459].
- In October 2026 they wrote that `OpenAsync` "is not yet fully asynchronous
  down to each individual network call" and that
  [future work will correct it][sqlclient-pool-v2]. They gave no date.
- The missing asynchronous commit and rollback are
  [known to them as well][sqlclient-2215].

Once those paths no longer block, the rest is ours to do: Isoplane has to
select the driver's managed implementation for its platform, which needs no
change to your code. We will recheck SQL Server as the driver's releases
arrive.

### SQLite {#sqlite}

[`Microsoft.Data.Sqlite`][Microsoft.Data.Sqlite] calls SQLite's native
library, which it ships built for each operating system and processor and for
browsers, but not for the sandbox. Creating a
[`SqliteConnection`][SqliteConnection] throws `TypeInitializationException`,
caused by `DllNotFoundException: e_sqlite3`. That includes in-memory
databases, and
[`Microsoft.EntityFrameworkCore.Sqlite`][nuget-ef-sqlite] with its
[`UseSqlite`][UseSqlite], which is built on it.

```csharp
// A common set-up for tests and prototypes:
await using var connection = new SqliteConnection("Data Source=:memory:");   // throws
await connection.OpenAsync();
```

Typically used for local development databases, tests and small applications.

#### Workaround: none for SQLite itself

Use PostgreSQL in development as well. The `isoplane-webapi` template includes a compose file for it; see the [project templates](reference/project-templates.md).

Size of the change: large: a different database, in development as
well as in production.

#### Looking ahead

SQLite can be supported. Native code compiled to WebAssembly runs inside the
sandbox, as the native parts of .NET already do in Isoplane's runtime (see
[the machine underneath](dotnet-runtime-differences.md#the-machine-underneath)).
The runtime does not include SQLite yet. With it, in-memory databases could work
unchanged. A database kept in a file would not: it needs storage that outlasts
the instance, which Isoplane does not provide.

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

We ran this against a router behind HAProxy, creating the client within the
request: inserts, queries and updates worked.

Few deployments can offer this. The routers have to be started with a
dedicated load-balancer port, behind a load balancer that sends the PROXY
protocol. In 2024 a MongoDB product manager described that arrangement as
[experimental for self-managed deployments][mongodb-load-balancer], not an
officially supported configuration, and Atlas does not offer it for sharded
clusters.

Size of the change: small in code, but for most applications it also means a
different deployment, one MongoDB does not support for production.

#### Looking ahead

Every other deployment waits on the driver's threads. Its maintainers moved
monitoring onto dedicated threads [on purpose][csharp-3761], and in 2023
[declined a request to use fewer][csharp-4661]. Nobody has yet asked them to
support a runtime without threads, so there is no work under way to cite.

## Caching

### Redis {#redis}

[`StackExchange.Redis`][StackExchange.Redis] does not work in its stable
releases yet, for a different reason in each line of them:

- **2.x**, which is what
  [`AddStackExchangeRedisCache`][AddStackExchangeRedisCache] from
  `Microsoft.Extensions.Caching.StackExchangeRedis` brings in, starts threads
  for its connections. `ConnectionMultiplexer.ConnectAsync` throws
  [`PlatformNotSupportedException`][PlatformNotSupportedException] from
  [`Thread.Start`][Thread.Start].
- **3.x**, the current stable line, [rewrote its I/O][se-redis-3] and starts
  no threads on its normal path. It fails on one call instead: it tests a
  free lock with `SemaphoreSlim.Wait(0)`, which
  [throws on Isoplane even when nothing would wait](dotnet-runtime-differences.md#one-thread).

```csharp
builder.Services.AddStackExchangeRedisCache(options =>
    options.Configuration = builder.Configuration.GetConnectionString("Redis"));   // fails when the cache is first used
```

Typically used for a cache shared between instances, session state, and Data
Protection keys.

#### Workaround: the client's 4.0 prerelease, through its asynchronous API

Version [4.0 of the client][se-redis-4], which is in alpha, works. Add a direct
reference to it, which replaces the 2.x that the Microsoft package would bring
in, and leave the rest of the application as it is:

```xml
<PackageReference Include="StackExchange.Redis" Version="4.0.127-alpha" />
<PackageReference Include="Microsoft.Extensions.Caching.StackExchangeRedis" Version="10.0.13" />
```

With that in place these ran as they do elsewhere:
[`IDistributedCache`][IDistributedCache] through `AddStackExchangeRedisCache`,
[`HybridCache`][HybridCache] with Redis as its second level, session state
(with the [middleware that loads it](#session)), and the client used directly:

```csharp
await using var redis = await ConnectionMultiplexer.ConnectAsync(connectionString);
var db = redis.GetDatabase();
await db.StringSetAsync("greeting", "kia ora", TimeSpan.FromSeconds(30));
var greeting = (string?)await db.StringGetAsync("greeting");
```

What to know before relying on it:

- **It is a prerelease.** Its maintainers describe the 4.0 work as not ready
  to merge.
- **Use the asynchronous methods only.** `ConnectionMultiplexer.Connect`
  throws. A synchronous command such as `db.StringGet(...)` does not throw: it
  never returns, and the request runs until its
  [time limit](execution-limits.md#running-time).
- **Connect inside a request, not during initialization.** The prerelease
  leaves a timer running even after it is disposed, which initialization
  refuses.
- **Each request reconnects.** The client keeps its connection open, and a
  request that ends with a connection open is
  [followed by a fresh instance](dotnet-runtime-differences.md#work-belongs-to-a-request).
- **The Redis output cache store does not start**
  ([`AddStackExchangeRedisOutputCache`][AddStackExchangeRedisOutputCache]): it
  runs a clean-up loop on a timer from start-up, whichever client is used.

We ran this against Redis 7 without TLS or authentication. Clusters, sentinel
and [`PersistKeysToStackExchangeRedis`][PersistKeysToStackExchangeRedis] were
not run.

Size of the change: small, with a prerelease dependency to carry.

#### Looking ahead

The stable 3.x client is one call away from working, and that call is in
Isoplane's runtime, not in the client. We ran 3.4.5 built in a way that avoids
`SemaphoreSlim.Wait(0)`, and everything above passed, including connecting
during initialization. When the runtime lets a free semaphore be taken without
throwing, as .NET itself is [being asked to][runtime-134972], the stable
client is expected to work with no change to your code.

## Observability

### OpenTelemetry SDK {#opentelemetry}

An application that sets up the OpenTelemetry SDK at start-up refuses to
start: `initialization retains a timer`. Tracing, metrics and logging all fail
this way, with any exporter, because one of the timers belongs to the SDK
itself.

```csharp
builder.Services.AddOpenTelemetry()
    .WithTracing(tracing => tracing
        .AddAspNetCoreInstrumentation()
        .AddOtlpExporter());   // the application refuses to start
```

The refusal names the timers. For the code above there are two:

- A delay of 10 s awaited by the SDK's self-diagnostics, which look for a
  configuration file. It starts with the SDK, and no setting switches it off.
- A delay of 5 s awaited by the batch exporter. A metrics reader has one of
  60 s in its place.

Creating the SDK later, after initialization, does not get round this. The
application then starts, but those timers are still pending when each request
ends, which [costs the instance](dotnet-runtime-differences.md#work-belongs-to-a-request).
And the OTLP exporter sends synchronously, over HTTP and over gRPC alike, so
nothing arrives.

Typically used for sending traces, metrics and logs to a collector. We ran
OpenTelemetry 1.19.1.

#### Workaround: trace without the SDK, and send spans before the response completes

.NET's tracing does not need the OpenTelemetry SDK. ASP.NET Core starts an
[`Activity`][Activity] for each request, your own code can start more from an
`ActivitySource`, and an [`ActivityListener`][ActivityListener] is handed each
one as it ends. A middleware can then send what was collected to a collector
as [OTLP over HTTP][otlp-spec], in its JSON encoding, before the response
completes. An incoming `traceparent` header is honoured as usual.

```csharp
using System.Diagnostics;
using System.Text;
using Microsoft.AspNetCore.Http.Features;

var endpoint = builder.Configuration["Otlp:Endpoint"]!;   // for example http://collector:4318/v1/traces

// Spans that have ended and not been sent yet.
var finished = new List<Activity>();
ActivitySource.AddActivityListener(new ActivityListener
{
    ShouldListenTo = source => source.Name is "Microsoft.AspNetCore" or "MyApp",
    Sample = (ref ActivityCreationOptions<ActivityContext> _) => ActivitySamplingResult.AllDataAndRecorded,
    ActivityStopped = finished.Add,
});

var app = builder.Build();

app.Use(async (context, next) =>
{
    try
    {
        await next(context);
    }
    finally
    {
        // ASP.NET Core ends the request's own span after the response is complete, which is too
        // late to send it. End it here, with the name and attributes it would be given then.
        if (context.Features.Get<IHttpActivityFeature>()?.Activity is { } server)
        {
            var route = (context.GetEndpoint() as RouteEndpoint)?.RoutePattern.RawText;
            server.DisplayName = route is null ? context.Request.Method : $"{context.Request.Method} {route}";
            server.SetTag("http.request.method", context.Request.Method);
            server.SetTag("url.path", context.Request.Path.Value);
            if (route is not null) server.SetTag("http.route", route);
            server.SetTag("http.response.status_code", context.Response.StatusCode);
            if (context.Response.StatusCode >= 500) server.SetStatus(ActivityStatusCode.Error);
            server.Stop();
        }

        if (finished.Count > 0)
        {
            var spans = finished.ToArray();
            finished.Clear();
            try
            {
                // Created and disposed within the request, so that nothing is left pending when it ends.
                using var client = new HttpClient();
                using var content = new StringContent(OtlpJson.Encode(spans, "my-app"), Encoding.UTF8, "application/json");
                using var response = await client.PostAsync(endpoint, content, context.RequestAborted);
                response.EnsureSuccessStatusCode();
            }
            catch (Exception error)
            {
                app.Logger.LogWarning(error, "{Count} span(s) were not exported", spans.Length);
            }
        }
    }
});
```

<details>
<summary>The <code>OtlpJson</code> encoder used above</summary>

```csharp
using System.Diagnostics;
using System.Text.Json.Nodes;

/// The OTLP/HTTP JSON encoding of a batch of spans.
static class OtlpJson
{
    public static string Encode(IEnumerable<Activity> spans, string service)
    {
        var encoded = new JsonArray();
        foreach (var activity in spans)
        {
            var attributes = new JsonArray();
            foreach (var tag in activity.TagObjects) attributes.Add(Attribute(tag.Key, tag.Value));
            var span = new JsonObject
            {
                ["traceId"] = activity.TraceId.ToHexString(),
                ["spanId"] = activity.SpanId.ToHexString(),
                ["name"] = activity.DisplayName,
                ["kind"] = (int)activity.Kind + 1,
                ["startTimeUnixNano"] = Nanos(activity.StartTimeUtc),
                ["endTimeUnixNano"] = Nanos(activity.StartTimeUtc + activity.Duration),
                ["attributes"] = attributes,
                ["status"] = new JsonObject { ["code"] = (int)activity.Status },
            };
            if (activity.ParentSpanId != default) span["parentSpanId"] = activity.ParentSpanId.ToHexString();
            encoded.Add(span);
        }
        return new JsonObject
        {
            ["resourceSpans"] = new JsonArray(new JsonObject
            {
                ["resource"] = new JsonObject { ["attributes"] = new JsonArray(Attribute("service.name", service)) },
                ["scopeSpans"] = new JsonArray(new JsonObject { ["scope"] = new JsonObject { ["name"] = service }, ["spans"] = encoded }),
            }),
        }.ToJsonString();
    }

    static string Nanos(DateTime utc) => ((utc - DateTime.UnixEpoch).Ticks * 100).ToString();

    static JsonNode Attribute(string key, object? value) => new JsonObject
    {
        ["key"] = key,
        ["value"] = value switch
        {
            bool flag => new JsonObject { ["boolValue"] = flag },
            int or long or short or byte => new JsonObject { ["intValue"] = Convert.ToInt64(value).ToString() },
            float or double => new JsonObject { ["doubleValue"] = Convert.ToDouble(value) },
            _ => new JsonObject { ["stringValue"] = value?.ToString() ?? "" },
        },
    };
}
```

</details>

What this gives up, compared with the SDK:

- **Batching.** There is one export for each request, on a new connection.
- **Retries.** If the collector refuses an export, the request still succeeds
  and those spans are dropped, with a warning in the log.
- **Response time.** The response does not complete until the export has. A
  collector that takes a second to answer adds a second to every request.
- **The SDK's features:** samplers, resource detectors, the instrumentation
  packages, and the protobuf and gRPC encodings.
- **Metrics and logs.** This covers traces only.

We checked the output against a receiver that records what it is sent, not
against a collector.

Size of the change: large.

#### Looking ahead

The SDK is close. Its maintainers already support single-threaded WebAssembly
in the browser: the batch exporter and the metrics reader have
[task-based forms for it][otel-6379], and the OTLP exporter was
[adjusted for it in 1.19.0][otel-7708]. Two things remain for Isoplane: a way
to keep the self-diagnostics from starting a timer, and an export that does
not send synchronously, which the maintainers have so far left out.

Isoplane can also close the gap from its own side, with an exporter that
collects spans and sends them before the response completes, or hands them to
the host to send in batches outside the request. Which of these comes first is
not yet known.

## Model Context Protocol

### An MCP server that tracks sessions {#mcp-sessions}

[`ModelContextProtocol.AspNetCore`][nuget-mcp] serves the Model Context
Protocol over Streamable HTTP. In its [stateless mode][mcp-stateless] it works.
With session tracking switched on, the package registers a background service,
and the application refuses to start with an error that names
`ModelContextProtocol.AspNetCore.IdleTrackingBackgroundService`.

```csharp
builder.Services
    .AddMcpServer()
    .WithHttpTransport(options => options.Stateless = false)   // the application refuses to start
    .WithTools<HandbookTools>();
```

Typically used for servers that start exchanges of their own with a client:
sampling, elicitation and unsolicited notifications.

#### Workaround: use stateless mode

Leave [`Stateless`][mcp-http-options] set to `true` on the
HTTP transport options, which is its default in version 2.2.0. Every request
then stands alone: the server answers calls, and cannot start an exchange of
its own. The
[`isoplane-mcpserver`](reference/project-templates.md#isoplane-mcpserver)
template is a server set up this way.

Size of the change: small for a server that only answers tool calls. A
server that depends on sampling or elicitation has no workaround.

#### Looking ahead

Session tracking keeps each session in memory between
requests and tidies up from a background service. This session-tracking mode is not supported.

## Packages that work {#works}

These ran in the sandbox as they would anywhere else. Each entry says what we
exercised, which is not everything the package does.

- **ASP.NET Core 10.0.** Minimal APIs, MVC controllers and Razor views, Razor
  Pages with tag helpers, model validation and TempData kept in a cookie, JSON
  bodies of 100 KB in both directions, URL-encoded forms of several megabytes,
  antiforgery tokens, output caching, response compression and health checks.
- **ASP.NET Core Identity** with
  `Microsoft.AspNetCore.Identity.EntityFrameworkCore` 10.0.12. Creating a
  user, password sign-in, and an authorized request with the sign-in cookie.
- **`Microsoft.AspNetCore.Authentication.JwtBearer` 10.0.12.** Validating a
  token against signing keys fetched from
  [`MetadataAddress`][JwtBearerOptions.MetadataAddress].
- **`Microsoft.AspNetCore.Authentication.OpenIdConnect` 10.0.12.** The sign-in
  redirect, which fetches the provider's metadata. The callback was not
  exercised.
- **`Microsoft.EntityFrameworkCore` 10.0.12 with
  `Npgsql.EntityFrameworkCore.PostgreSQL` 10.0.3.** Asynchronous queries,
  saves, transactions and migrations, the retrying execution strategy, and
  context pooling.
- **`Microsoft.Extensions.Diagnostics.HealthChecks.EntityFrameworkCore`
  10.0.12.** [`AddDbContextCheck`][AddDbContextCheck].
- **[`MySqlConnector`][MySqlConnector] 2.6.2.** Opening a connection to
  MySQL and running a query. Transactions and the Entity Framework Core
  providers built on this driver have not been run.
- **[`Dapper`][dapper] 2.1.89** with Npgsql. `QueryAsync`, `QueryFirstAsync` over a
  large result, and `ExecuteAsync`.
- **`Microsoft.Extensions.Caching.Hybrid` 10.10.0.** [`HybridCache.GetOrCreateAsync`][HybridCache.GetOrCreateAsync],
  with and without a distributed second level.
- **`Microsoft.Extensions.Http.Resilience` 10.10.0.** A request through
  [`AddStandardResilienceHandler`][AddStandardResilienceHandler].
- **[`Serilog.AspNetCore`][serilog-aspnetcore] 10.0.0.** `UseSerilog` with the
  [console sink][serilog-console].
- **[`FluentValidation`][fluentvalidation] 12.2.0.** Synchronous and
  asynchronous rules through [`ValidateAsync`][fluentvalidation-async].
- **[`MediatR`][mediatr] 12.5.0.** `IMediator.Send` to a handler that queries a database.
- **[`AutoMapper`][automapper] 14.0.0.** Mapping an entity to a record.
- **[`ModelContextProtocol.AspNetCore`][nuget-mcp] 2.2.0.** Tools called
  through the MCP client, over stateless Streamable HTTP.
- **[`Swashbuckle.AspNetCore`][swashbuckle] 10.3.0.** Generating the OpenAPI document. The
  Swagger UI was not exercised.

With Npgsql, turn the driver's connection pooling off:
[Pool database connections outside the application](dotnet-runtime-differences.md#pool-database-connections-outside-the-application).

## Packages not checked yet {#not-checked}

We have not run these, so this page makes no claim about them. Where the
general rules suggest what to expect, the entry says so.

- **Azure and AWS SDK clients**, such as `Azure.Storage.Blobs` and
  `AWSSDK.S3`. Their asynchronous methods are built on [`HttpClient`][HttpClient], whose
  asynchronous methods work, but the clients themselves have not been run.
  Their synchronous methods block, as
  [any synchronous I/O does](dotnet-runtime-differences.md#waiting-is-asynchronous).
- **[`Pomelo.EntityFrameworkCore.MySql`][Pomelo]** and other Entity Framework
  Core providers for MySQL. The driver beneath them,
  [`MySqlConnector`][MySqlConnector], [connects and queries](#works).
- **`Microsoft.Identity.Web`.**
- **`MailKit`.** It builds into an application package. Sending mail has not
  been run.
- **`Grpc.Net.Client`**, and Azure Cosmos DB.
- **Job schedulers and message consumers**, such as Hangfire, Quartz.NET and
  MassTransit. They rely on
  [work between requests](dotnet-runtime-differences.md#work-belongs-to-a-request).

To look into a package yourself, build with
[`IsoplaneAnalyzeDependencies`](reference/project-settings.md#isoplaneanalyzedependencies)
set. The build then names every package assembly that contains a call that
starts a thread or waits synchronously. Treat the result as a list of things
to try, not a verdict: it flags an assembly for a call anywhere inside it, and
that includes Npgsql and Entity Framework Core, whose asynchronous paths work.

[Activity]: https://learn.microsoft.com/dotnet/api/system.diagnostics.activity
[ActivityListener]: https://learn.microsoft.com/dotnet/api/system.diagnostics.activitylistener
[AddAzureAppConfiguration]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.configuration.azureappconfigurationextensions.addazureappconfiguration
[AddAzureKeyVault]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.configuration.azurekeyvaultconfigurationextensions.addazurekeyvault
[AddConsole]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.consoleloggerextensions.addconsole
[AddDbContextCheck]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.entityframeworkcorehealthchecksbuilderextensions.adddbcontextcheck
[AddFixedWindowLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.ratelimiting.ratelimiteroptionsextensions.addfixedwindowlimiter
[AddInMemoryCollection]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.configuration.memoryconfigurationbuilderextensions.addinmemorycollection
[AddJsonConsole]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.consoleloggerextensions.addjsonconsole
[AddJsonOptions]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.mvccoremvcbuilderextensions.addjsonoptions
[AddNewtonsoftJson]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.newtonsoftjsonmvcbuilderextensions.addnewtonsoftjson
[AddRateLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.builder.ratelimiterservicecollectionextensions.addratelimiter
[AddSession]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.sessionservicecollectionextensions.addsession
[AddSessionStateTempDataProvider]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.mvcviewfeaturesmvcbuilderextensions.addsessionstatetempdataprovider
[AddStackExchangeRedisCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.stackexchangerediscacheservicecollectionextensions.addstackexchangerediscache
[AddStackExchangeRedisOutputCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.stackexchangeredisoutputcacheservicecollectionextensions.addstackexchangeredisoutputcache
[AddStandardResilienceHandler]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.resiliencehttpclientbuilderextensions.addstandardresiliencehandler
[AddXmlSerializerFormatters]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.mvcxmlmvcbuilderextensions.addxmlserializerformatters
[AsFileSection]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.webutilities.multipartsectionconverterextensions.asfilesection
[automapper]: https://docs.automapper.io/
[BackgroundService]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.hosting.backgroundservice
[BeginTransactionAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.infrastructure.databasefacade.begintransactionasync
[ClearProviders]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.loggingbuilderextensions.clearproviders
[CommitAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.storage.idbcontexttransaction.commitasync
[ConfigureHttpJsonOptions]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.httpjsonserviceextensions.configurehttpjsonoptions
[Controller.TempData]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.controller.tempdata
[CryptographicException]: https://learn.microsoft.com/dotnet/api/system.security.cryptography.cryptographicexception
[CryptoStream]: https://learn.microsoft.com/dotnet/api/system.security.cryptography.cryptostream
[csharp-3761]: https://jira.mongodb.org/browse/CSHARP-3761
[csharp-4661]: https://jira.mongodb.org/browse/CSHARP-4661
[dapper]: https://www.learndapper.com/
[DbConnection.OpenAsync]: https://learn.microsoft.com/dotnet/api/system.data.common.dbconnection.openasync
[DbContext.SaveChanges]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbcontext.savechanges
[DbContext.SaveChangesAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbcontext.savechangesasync
[DllNotFoundException]: https://learn.microsoft.com/dotnet/api/system.dllnotfoundexception
[EnableBuffering]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequestrewindextensions.enablebuffering
[FixedWindowRateLimiter]: https://learn.microsoft.com/dotnet/api/system.threading.ratelimiting.fixedwindowratelimiter
[fluentvalidation]: https://docs.fluentvalidation.net/
[fluentvalidation-async]: https://docs.fluentvalidation.net/en/latest/async.html
[FormOptions]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions
[FormOptions.MemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions.memorybufferthreshold
[FormOptions.MultipartBodyLengthLimit]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions.multipartbodylengthlimit
[HttpClient]: https://learn.microsoft.com/dotnet/api/system.net.http.httpclient
[HttpContext.Session]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httpcontext.session
[HttpRequest.ReadFormAsync]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequest.readformasync
[HybridCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.caching.hybrid.hybridcache
[HybridCache.GetOrCreateAsync]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.caching.hybrid.hybridcache.getorcreateasync
[IConfigurationProvider.Load]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.configuration.iconfigurationprovider.load
[IDataProtectionBuilder]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.idataprotectionbuilder
[IDistributedCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.caching.distributed.idistributedcache
[IFormFile]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.iformfile
[ILazyLoader]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.infrastructure.ilazyloader
[ILogger]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.ilogger
[Include]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.entityframeworkqueryableextensions.include
[InputFormatterMemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.mvcnewtonsoftjsonoptions.inputformattermemorybufferthreshold
[ISession.LoadAsync]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.isession.loadasync
[IssuerSigningKeyResolver]: https://learn.microsoft.com/dotnet/api/microsoft.identitymodel.tokens.tokenvalidationparameters.issuersigningkeyresolver
[IStringLocalizer]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.localization.istringlocalizer
[IValidatableObject.Validate]: https://learn.microsoft.com/dotnet/api/system.componentmodel.dataannotations.ivalidatableobject.validate
[IXmlRepository]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.repositories.ixmlrepository
[JwtBearerOptions.Authority]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.authentication.jwtbearer.jwtbeareroptions.authority
[JwtBearerOptions.MetadataAddress]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.authentication.jwtbearer.jwtbeareroptions.metadataaddress
[mcp-http-options]: https://csharp.sdk.modelcontextprotocol.io/v2/api/ModelContextProtocol.AspNetCore.HttpServerTransportOptions.html
[mcp-stateless]: https://csharp.sdk.modelcontextprotocol.io/v2/concepts/stateless/stateless.html
[mediatr]: https://github.com/jbogard/MediatR
[Microsoft.Data.Sqlite]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlite
[MongoDB.Driver]: https://www.mongodb.com/docs/drivers/csharp/current/
[mongodb-load-balancer]: https://dev.to/alexbevi/mongodb-and-load-balancer-support-1p28
[MultipartReader]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.webutilities.multipartreader
[MySqlConnector]: https://mysqlconnector.net/
[NetworkStream]: https://learn.microsoft.com/dotnet/api/system.net.sockets.networkstream
[nuget-aws-ssm]: https://www.nuget.org/packages/Amazon.Extensions.Configuration.SystemsManager
[nuget-dp-ef]: https://www.nuget.org/packages/Microsoft.AspNetCore.DataProtection.EntityFrameworkCore
[nuget-ef-proxies]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.Proxies
[nuget-ef-sqlite]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.Sqlite
[nuget-ef-sqlserver]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.SqlServer
[nuget-mcp]: https://www.nuget.org/packages/ModelContextProtocol.AspNetCore
[nuget-newtonsoft]: https://www.nuget.org/packages/Microsoft.AspNetCore.Mvc.NewtonsoftJson
[nuget-sqlclient]: https://www.nuget.org/packages/Microsoft.Data.SqlClient
[otel-6379]: https://github.com/open-telemetry/opentelemetry-dotnet/pull/6379
[otel-7708]: https://github.com/open-telemetry/opentelemetry-dotnet/issues/7708
[otlp-spec]: https://opentelemetry.io/docs/specs/otlp/
[OutputFormatterMemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.mvcnewtonsoftjsonoptions.outputformattermemorybufferthreshold
[PersistKeysToDbContext]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.entityframeworkcoredataprotectionextensions.persistkeystodbcontext
[PersistKeysToFileSystem]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.dataprotectionbuilderextensions.persistkeystofilesystem
[PersistKeysToStackExchangeRedis]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.stackexchangeredisdataprotectionbuilderextensions.persistkeystostackexchangeredis
[PlatformNotSupportedException]: https://learn.microsoft.com/dotnet/api/system.platformnotsupportedexception
[Pomelo]: https://github.com/PomeloFoundation/Pomelo.EntityFrameworkCore.MySql
[rdg]: https://learn.microsoft.com/aspnet/core/fundamentals/aot/request-delegate-generator/rdg
[ReferenceEntry.LoadAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.changetracking.referenceentry.loadasync
[runtime-134972]: https://github.com/dotnet/runtime/issues/134972
[se-redis-3]: https://github.com/StackExchange/StackExchange.Redis/releases/tag/3.0.0
[se-redis-4]: https://github.com/StackExchange/StackExchange.Redis/pull/3262
[SemaphoreSlim.Wait]: https://learn.microsoft.com/dotnet/api/system.threading.semaphoreslim.wait
[serilog-aspnetcore]: https://github.com/serilog/serilog-aspnetcore
[serilog-console]: https://github.com/serilog/serilog-sinks-console
[Socket]: https://learn.microsoft.com/dotnet/api/system.net.sockets.socket
[sqlclient-2215]: https://github.com/dotnet/SqlClient/issues/2215
[sqlclient-3459]: https://github.com/dotnet/SqlClient/issues/3459
[sqlclient-979]: https://github.com/dotnet/SqlClient/issues/979
[sqlclient-pool-v2]: https://devblogs.microsoft.com/azure-sql/try-sqlclients-new-connection-pool-for-faster-parallel-connections/
[SqlConnection]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlclient.sqlconnection
[SqliteConnection]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlite.sqliteconnection
[StackExchange.Redis]: https://stackexchange.github.io/StackExchange.Redis/
[stj-source-generation]: https://learn.microsoft.com/dotnet/standard/serialization/system-text-json/source-generation
[swashbuckle]: https://github.com/domaindrivendev/Swashbuckle.AspNetCore
[System.Text.Json]: https://learn.microsoft.com/dotnet/api/system.text.json
[Task]: https://learn.microsoft.com/dotnet/api/system.threading.tasks.task
[Task.Result]: https://learn.microsoft.com/dotnet/api/system.threading.tasks.task-1.result
[Thread.Sleep]: https://learn.microsoft.com/dotnet/api/system.threading.thread.sleep
[Thread.Start]: https://learn.microsoft.com/dotnet/api/system.threading.thread.start
[TokenBucketRateLimiter]: https://learn.microsoft.com/dotnet/api/system.threading.ratelimiting.tokenbucketratelimiter
[TokenValidationParameters]: https://learn.microsoft.com/dotnet/api/microsoft.identitymodel.tokens.tokenvalidationparameters
[TransactionInDoubtException]: https://learn.microsoft.com/dotnet/api/system.transactions.transactionindoubtexception
[TransactionScope]: https://learn.microsoft.com/dotnet/api/system.transactions.transactionscope
[TryReplenish]: https://learn.microsoft.com/dotnet/api/system.threading.ratelimiting.replenishingratelimiter.tryreplenish
[UseLazyLoadingProxies]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.proxiesextensions.uselazyloadingproxies
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
