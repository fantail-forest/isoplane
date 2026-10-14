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
asynchronous. It does not repeat the general rules. If your code calls a
synchronous method such as Entity Framework Core's
[`SaveChanges`][DbContext.SaveChanges], or waits on a task with
[`Task.Result`][Task.Result],
[Waiting is asynchronous](dotnet-runtime-differences.md#waiting-is-asynchronous)
covers it.

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
| ASP.NET Core | [Rate limiting middleware](#rate-limiting) | Large change: limit requests elsewhere |
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
| Sqlite | [SQLite, including its Entity Framework Core provider](#sqlite) | Cannot be supported: use another database |
| MongoDB.Driver | [MongoDB](#mongodb) | Not supported: use another database |
| StackExchange.Redis | [Redis, including the Redis distributed cache](#redis) | Not supported: use another cache store |
| OpenTelemetry | [OpenTelemetry tracing](#opentelemetry) | No workaround yet |
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

An application that calls [`UseRateLimiter`][UseRateLimiter] with a
fixed-window policy refuses to start: `initialization retains a timer`. The
limiter starts a timer that ticks every 100 ms when the application starts,
and a timer cannot be carried from initialization into an instance. Other policies have not been
checked.

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

#### Workaround: limit requests outside the application

Count requests in a store that every instance shares, or limit them in front of the application.

Size of the change: large. Limiting moves out of the application, or onto
a store the instances share.

#### Looking ahead

A change to Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`,
can let an application with this middleware start. Its limiter would still
count for one instance at a time, because its counters are in memory and
[memory belongs to one instance](dotnet-runtime-differences.md#instances-come-and-go),
so a shared store remains the place to count.

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

[`Microsoft.Data.Sqlite`][Microsoft.Data.Sqlite] calls a native SQLite
library, which the sandbox does not load. Creating a
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

No fix is possible. SQLite is native code, which would
run outside the sandbox, and
[nothing is allowed around it](dotnet-runtime-differences.md#nothing-around-the-sandbox).

### MongoDB {#mongodb}

[`MongoDB.Driver`][MongoDB.Driver] starts a thread for each server it
monitors. The first operation throws
[`PlatformNotSupportedException`][PlatformNotSupportedException] from
[`Thread.Start`][Thread.Start].

```csharp
var client = new MongoClient(connectionString);
var orders = client.GetDatabase("shop").GetCollection<Order>("orders");
await orders.InsertOneAsync(order);   // the first operation: throws
```

Typically used for MongoDB and compatible document databases.

#### Workaround: none

There is no way to use MongoDB from an application today.

Size of the change: large: a different database.

#### Looking ahead

MongoDB will work when its driver can monitor its
servers without threads of its own, which is a change for the driver's
maintainers.

## Caching

### Redis {#redis}

[`StackExchange.Redis`][StackExchange.Redis] starts threads for its
connections, so [`ConnectionMultiplexer.Connect`][se-redis-basics] and
`ConnectAsync` throw
[`PlatformNotSupportedException`][PlatformNotSupportedException] from
[`Thread.Start`][Thread.Start].

```csharp
builder.Services.AddSingleton<IConnectionMultiplexer>(
    ConnectionMultiplexer.Connect(builder.Configuration.GetConnectionString("Redis")!));   // throws

builder.Services.AddStackExchangeRedisCache(options =>
    options.Configuration = builder.Configuration.GetConnectionString("Redis"));   // connects, and fails, on first use
```

Configuring the client with `SocketManager.ThreadPool`, a
[configuration option][se-redis-configuration] in the `StackExchange.Redis`
namespace, avoids the threads but does not help: the connection opens, the
handshake never completes, and `ConnectAsync` fails when its timeout passes.
With the Redis cache registered that way as the session store, every request
stalled.

Everything built on the client fails with it:
[`AddStackExchangeRedisCache`][AddStackExchangeRedisCache] from
`Microsoft.Extensions.Caching.StackExchangeRedis`, ASP.NET Core's Redis output
cache store
([`AddStackExchangeRedisOutputCache`][AddStackExchangeRedisOutputCache]), and
Data Protection's
[`PersistKeysToStackExchangeRedis`][PersistKeysToStackExchangeRedis].

Typically used for a cache shared between instances, session state, and
Data Protection keys.

#### Workaround: none with this client

For Data Protection
keys, see [Data Protection key stores](#data-protection).

Size of the change: large: a different store for the cache.

#### Looking ahead

Redis will work when the client can run without
threads of its own, which is a change for its maintainers. Why its
thread-pool configuration stalls is not yet known.

## Observability

### OpenTelemetry tracing {#opentelemetry}

An application that adds OpenTelemetry tracing refuses to start:
`initialization retains a timer`. We ran it with ASP.NET Core instrumentation
and the OTLP exporter, from the
[`OpenTelemetry.Exporter.OpenTelemetryProtocol`][otlp-exporter] package, and
two timers were still running when initialization ended:

- One belongs to the OpenTelemetry SDK's self-diagnostics, which look for a
  configuration file every ten seconds. It starts with the SDK, whichever
  exporter is used.
- One belongs to the batch exporter, which wakes every five seconds to send
  what it has collected.

```csharp
builder.Services.AddOpenTelemetry()
    .WithTracing(tracing => tracing
        .AddAspNetCoreInstrumentation()
        .AddOtlpExporter());   // the application refuses to start
```

Typically used for sending traces, metrics and logs to a collector.

#### Workaround: none yet

We have not found one.

#### Looking ahead

Not yet known. Both timers belong to loops that expect to
keep running, and sending in batches after the response is
[work between requests](dotnet-runtime-differences.md#work-belongs-to-a-request),
so an exporter would have to send within the request instead.

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
- **Rate limiting policies other than fixed window.**
- **OpenTelemetry metrics and logs, and exporters other than OTLP.** The
  self-diagnostics timer described under
  [OpenTelemetry tracing](#opentelemetry) starts with the SDK, so these are
  expected to fail the same way.

To look into a package yourself, build with
[`IsoplaneAnalyzeDependencies`](reference/project-settings.md#isoplaneanalyzedependencies)
set. The build then names every package assembly that contains a call that
starts a thread or waits synchronously. Treat the result as a list of things
to try, not a verdict: it flags an assembly for a call anywhere inside it, and
that includes Npgsql and Entity Framework Core, whose asynchronous paths work.

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
[dapper]: https://www.learndapper.com/
[DbConnection.OpenAsync]: https://learn.microsoft.com/dotnet/api/system.data.common.dbconnection.openasync
[DbContext.SaveChanges]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbcontext.savechanges
[DllNotFoundException]: https://learn.microsoft.com/dotnet/api/system.dllnotfoundexception
[EnableBuffering]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequestrewindextensions.enablebuffering
[fluentvalidation]: https://docs.fluentvalidation.net/
[fluentvalidation-async]: https://docs.fluentvalidation.net/en/latest/async.html
[FormOptions]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions
[FormOptions.MemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions.memorybufferthreshold
[FormOptions.MultipartBodyLengthLimit]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions.multipartbodylengthlimit
[HttpClient]: https://learn.microsoft.com/dotnet/api/system.net.http.httpclient
[HttpContext.Session]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httpcontext.session
[HttpRequest.ReadFormAsync]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequest.readformasync
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
[otlp-exporter]: https://github.com/open-telemetry/opentelemetry-dotnet/blob/main/src/OpenTelemetry.Exporter.OpenTelemetryProtocol/README.md
[OutputFormatterMemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.mvcnewtonsoftjsonoptions.outputformattermemorybufferthreshold
[PersistKeysToDbContext]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.entityframeworkcoredataprotectionextensions.persistkeystodbcontext
[PersistKeysToFileSystem]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.dataprotectionbuilderextensions.persistkeystofilesystem
[PersistKeysToStackExchangeRedis]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.stackexchangeredisdataprotectionbuilderextensions.persistkeystostackexchangeredis
[PlatformNotSupportedException]: https://learn.microsoft.com/dotnet/api/system.platformnotsupportedexception
[Pomelo]: https://github.com/PomeloFoundation/Pomelo.EntityFrameworkCore.MySql
[rdg]: https://learn.microsoft.com/aspnet/core/fundamentals/aot/request-delegate-generator/rdg
[ReferenceEntry.LoadAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.changetracking.referenceentry.loadasync
[se-redis-basics]: https://stackexchange.github.io/StackExchange.Redis/Basics
[se-redis-configuration]: https://stackexchange.github.io/StackExchange.Redis/Configuration
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
[TokenValidationParameters]: https://learn.microsoft.com/dotnet/api/microsoft.identitymodel.tokens.tokenvalidationparameters
[TransactionInDoubtException]: https://learn.microsoft.com/dotnet/api/system.transactions.transactionindoubtexception
[TransactionScope]: https://learn.microsoft.com/dotnet/api/system.transactions.transactionscope
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
