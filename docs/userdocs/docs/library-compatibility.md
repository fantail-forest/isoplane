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
synchronous method such as [`SaveChanges`][DbContext.SaveChanges], or waits on
a task with [`.Result`][Task.Result],
[Waiting is asynchronous](dotnet-runtime-differences.md#waiting-is-asynchronous)
covers it.

Each entry says what fails, how to work around it today and how big a change
that is, and whether a fix is possible. A fix here means that your code then
runs as you wrote it. Anything that asks you to change code is a workaround,
including a replacement method that Isoplane provides.
[Packages that work](#works) and [packages not checked yet](#not-checked) are
at the end.

Everything here was run in the sandbox with SDK 0.1.0-preview.4, unless its
entry says otherwise.

## At a glance {#summary}

| Package | API or feature | Workaround | Can it be fixed? |
| --- | --- | --- | --- |
| ASP.NET Core | [Session over a distributed cache, and TempData kept in it](#session) | Small: add one middleware | No fix known |
| ASP.NET Core | [`Request.Form`](#request-form) | Small: use `ReadFormAsync` | No: use the asynchronous form |
| ASP.NET Core | [File uploads over 64 KB](#uploads) | Moderate: read the upload as a stream | Yes, for uploads that fit in memory |
| ASP.NET Core | [`Request.EnableBuffering()` over 30 KB](#enable-buffering) | Small: pass a size limit | Yes, with no code change |
| ASP.NET Core | [Synchronous reads and writes of the request and response body](#synchronous-body) | Small: call the asynchronous method | No: use the asynchronous form |
| ASP.NET Core | [Data Protection key stores](#data-protection) | Small: use the replacement Isoplane provides | No: use the replacement |
| ASP.NET Core | [`Html.Partial` and `Html.RenderPartial`](#html-partial) | Small: use `<partial>` | No: use the asynchronous form |
| ASP.NET Core | [XML formatters over 30 KB](#xml-formatters) | None yet | Yes, with no code change |
| ASP.NET Core | [Rate limiting middleware](#rate-limiting) | Large: limit requests elsewhere | Partly |
| ASP.NET Core | [`AddConsole`, `AddJsonConsole` and `ClearProviders`](#console-logging) | Small: delete the calls | Yes, with no code change |
| ASP.NET Core | [Validation attributes, localizers, token callbacks and service factories that do I/O](#synchronous-extension-points) | Moderate: move the I/O elsewhere | No: it can only work by [blocking](dotnet-runtime-differences.md#never-blocks) |
| ASP.NET Core | [WebSockets, SignalR's WebSocket transport, Blazor Server](#connections-and-background-work) | None yet | Needs instances that stay connected |
| ASP.NET Core | [gRPC over HTTP/2](#connections-and-background-work) | Moderate: use gRPC-Web | Not known yet |
| ASP.NET Core | [`BackgroundService` and other work between requests](#connections-and-background-work) | Large: do the work during a request | Needs instances that stay running |
| Newtonsoft.Json for MVC | [Bodies over 30 KB](#newtonsoft-json) | Small: set two options | Yes, with no code change |
| Configuration providers | [Loading settings from Azure Key Vault and other remote services](#remote-configuration) | Moderate: fetch them before `Build()` | No: it can only work by [blocking](dotnet-runtime-differences.md#never-blocks) |
| Entity Framework Core | [Lazy loading](#lazy-loading) | Moderate: load with `Include` | No: it can only work by [blocking](dotnet-runtime-differences.md#never-blocks) |
| Entity Framework Core | [`UseHiLo` keys with `Add`](#hilo) | Small: use `AddAsync` | No: use the asynchronous form |
| Entity Framework Core | [`TransactionScope`](#transaction-scope) | Small: use a database transaction | Only in .NET itself |
| Npgsql | [Disposing a reader that has unread rows](#reader-dispose) | Small: use `await using` | No: use the asynchronous form |
| SqlClient | [SQL Server, including its Entity Framework Core provider](#sql-server) | Large: use another database | Only in the driver |
| MySqlConnector | [MySQL](#mysql) | None yet | Probably, with no code change |
| Sqlite | [SQLite, including its Entity Framework Core provider](#sqlite) | Large: use another database | No: it depends on [native code](dotnet-runtime-differences.md#nothing-around-the-sandbox) |
| MongoDB.Driver | [MongoDB](#mongodb) | Large: use another database | Only in the driver |
| StackExchange.Redis | [Redis, including the Redis distributed cache](#redis) | Large: use another cache store | Only in the client library |
| OpenTelemetry | [Exporting telemetry over OTLP](#opentelemetry) | None yet | Not known yet |

The sections below give each package's full name.

<details>
<summary>How the workarounds are sized</summary>

- **Small** is a one-for-one replacement: the asynchronous form of the same
  call, a setting, or a method Isoplane provides.
- **Moderate** is some restructuring, or a few lines of your own.
- **Large** is a different library or a different approach.

"Can it be fixed?" describes what is possible, not a schedule.

</details>

## Recognize a failure {#symptoms}

Exception messages from the runtime are short identifiers, not sentences, so
read the stack trace to see which call failed.

| What you see | What it means |
| --- | --- |
| `PlatformNotSupportedException: Arg_PlatformNotSupported`, with `Task.InternalWait` (internal to [`Task`][Task]), [`SemaphoreSlim.Wait`][SemaphoreSlim.Wait], [`Thread.Sleep`][Thread.Sleep], [`Socket`][Socket] or [`NetworkStream`][NetworkStream] near the top of the stack | Something waited synchronously. If the call is in your own code, see [Waiting is asynchronous](dotnet-runtime-differences.md#waiting-is-asynchronous). If it is inside a library, find the library in the table above. |
| `PlatformNotSupportedException: net_wasi_sync_name_resolution` | The same, for a connection that was opened synchronously. |
| `PlatformNotSupportedException: Arg_PlatformNotSupported`, with [`Thread.Start`][Thread.Start] near the top of the stack | A library started a thread. See [MongoDB](#mongodb) and [Redis](#redis). |
| `DirectoryNotFoundException: /tmp/` | ASP.NET Core tried to move a large request or response to a temporary file. See [uploads](#uploads), [`EnableBuffering`](#enable-buffering), [Newtonsoft.Json](#newtonsoft-json) and [XML formatters](#xml-formatters). |
| `IOException: Read-only file system` | Something tried to write a file. The [file system is read-only](dotnet-runtime-differences.md#the-machine-underneath). |
| [`DllNotFoundException`][DllNotFoundException], sometimes inside a `TypeInitializationException` | A package called into a native library. See [SQLite](#sqlite). |
| The application refuses to start with `initialization retains a timer` | Something registered at startup started a timer. See [rate limiting](#rate-limiting) and [OpenTelemetry](#opentelemetry). |
| The application refuses to start with `unhandled System.PlatformNotSupportedException` and no stack trace | A call made while the application starts failed. The known ones are [console logging](#console-logging) and [remote configuration](#remote-configuration). |
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
session with
[`AddSessionStateTempDataProvider`][AddSessionStateTempDataProvider].

- **Used for:** sign-in flows, shopping carts, multi-step forms, and messages
  shown after a redirect.
- **Workaround:** load the session with
  [`LoadAsync`][ISession.LoadAsync] before anything uses it, in a middleware
  placed directly after [`UseSession`][UseSession]:

  ```csharp
  app.UseSession();
  app.Use(async (context, next) =>
  {
      await context.Session.LoadAsync(context.RequestAborted);
      await next();
  });
  ```

- **Size of the change:** Small. One middleware, added once.
- **Can it be fixed?** No fix is known that would let the code above work as
  written. The workaround can get smaller: Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`,
  could provide this middleware, and report an unloaded session as an error in
  place of losing data.

We ran this with a PostgreSQL-backed cache. The Redis cache does not run at
all; see [Redis](#redis).

### `Request.Form` {#request-form}

Reading [`HttpRequest.Form`][HttpRequest.Form] before the form has been read
asynchronously throws
[`PlatformNotSupportedException`][PlatformNotSupportedException], whatever the
size of the body.

```csharp
app.MapPost("/contact", (HttpContext context) =>
{
    var email = context.Request.Form["email"];   // throws
    // ...
});
```

- **Used for:** reading posted fields and files directly from the request.
- **Workaround:** read the form with
  [`ReadFormAsync`][HttpRequest.ReadFormAsync]:
  `var form = await context.Request.ReadFormAsync();`. A minimal API handler
  that takes a form value or an [`IFormFile`][IFormFile] as a parameter already
  reads asynchronously.
- **Size of the change:** Small.
- **Can it be fixed?** No. Reading the form synchronously can only work by
  [blocking](dotnet-runtime-differences.md#never-blocks), so the asynchronous call is the lasting answer.

### File uploads over 64 KB {#uploads}

When ASP.NET Core reads a form, through
[`ReadFormAsync`][HttpRequest.ReadFormAsync] or an
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

- **Used for:** any multipart upload: images, documents, imports.
- **Workaround:** read each file as a stream while it arrives, with
  [`MultipartReader`][MultipartReader]. Isoplane hands the request body to the
  application as it reads, so a file of any size passes through in small pieces
  and is never held in memory.

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

  Raising [`MemoryBufferThreshold`][FormOptions.MemoryBufferThreshold] in
  [`FormOptions`][FormOptions] also gets `IFormFile` working, by holding each
  upload in memory. We do not recommend it wherever the upload can be read as a
  stream: every upload then counts toward the instance's
  [memory limit](execution-limits.md#memory), and the largest file the
  application can accept is whatever memory it has left. If you do use it, set
  [`MultipartBodyLengthLimit`][FormOptions.MultipartBodyLengthLimit] to the
  same size, so that a larger file is refused instead of buffered.
- **Size of the change:** Moderate. The handler reads the multipart body
  itself instead of taking an `IFormFile`.
- **Can it be fixed?** Yes, for uploads that fit in memory, with no change to your
  code: Isoplane can give ASP.NET Core somewhere in memory to buffer, so that
  `IFormFile` works as written. Reading a large upload as a stream stays the
  better choice.

Large fields of an ordinary URL-encoded form are not affected.

### `Request.EnableBuffering()` over 30 KB {#enable-buffering}

[`EnableBuffering`][EnableBuffering] lets a request body be read more than
once. It keeps 30 KB in memory and moves a larger body to a temporary file,
which fails with `DirectoryNotFoundException: /tmp/` when the body is read.

```csharp
context.Request.EnableBuffering();
var payload = await new StreamReader(context.Request.Body).ReadToEndAsync();   // throws over 30 KB
context.Request.Body.Position = 0;   // so that the body can be bound afterwards
```

- **Used for:** verifying a webhook signature before binding the body, and
  logging request bodies.
- **Workaround:** pass the largest body you accept as both the threshold
  and the limit. A body up to that size stays in memory, and a larger one is
  refused.

  ```csharp
  const int LargestBody = 1024 * 1024;

  context.Request.EnableBuffering(bufferThreshold: LargestBody, bufferLimit: LargestBody);
  ```

  The buffered body counts toward the instance's
  [memory limit](execution-limits.md#memory).
- **Size of the change:** Small, and temporary. It is two arguments.
- **Can it be fixed?** Yes, with no change to your code. Isoplane can give
  ASP.NET Core somewhere in memory to buffer.

### Synchronous reads and writes of the request and response body {#synchronous-body}

[`Request.Body`][HttpRequest.Body] and [`Response.Body`][HttpResponse.Body]
are streams whose synchronous [`Read`][Stream.Read] and
[`Write`][Stream.Write] throw
[`NotSupportedException`][NotSupportedException], with a message that says to
use the asynchronous form. ASP.NET Core's own server refuses these calls as
well, unless [`AllowSynchronousIO`][AllowSynchronousIO] is set.

```csharp
var serializer = new XmlSerializer(typeof(Report));
serializer.Serialize(context.Response.Body, report);   // writes synchronously: throws
```

- **Used for:** serializers and writers that take a stream and write to it
  synchronously, such as [`XmlSerializer.Serialize`][XmlSerializer.Serialize].
- **Workaround:** use [`ReadAsync`][Stream.ReadAsync] and
  [`WriteAsync`][Stream.WriteAsync], or the library's asynchronous method.
  Where a library has none, have it write to a
  [`MemoryStream`][MemoryStream] and copy that to the response with
  [`CopyToAsync`][Stream.CopyToAsync].
- **Size of the change:** Small.
- **Can it be fixed?** No, as on ASP.NET Core's own server. The asynchronous
  methods are the lasting answer.

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

- **Used for:** keeping authentication cookies and antiforgery tokens valid
  after a new build.
- **Workaround:** Isoplane provides replacements, described in
  [Keep Data Protection keys across builds](dotnet-runtime-differences.md#keep-data-protection-keys-across-builds).
  - `UseKeysFromDbContext` takes the place of `PersistKeysToDbContext` and uses
    the same table. It is an extension method on
    [`IDataProtectionBuilder`][IDataProtectionBuilder] in the
    `Isoplane.AspNetCore` namespace, which the SDK adds to an application that
    references the
    [`Microsoft.AspNetCore.DataProtection.EntityFrameworkCore`][nuget-dp-ef]
    package. It has no reference page yet.
  - [`UseConfiguredKeys`](reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/DataProtectionKeys/UseConfiguredKeys.md)
    derives the keys from a secret in the application's settings.
  - [`UseKeysFrom`](reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/DataProtectionKeys/UseKeysFrom.md)
    serves any other store.
- **Size of the change:** Small for Entity Framework Core, where one call
  replaces another. Moderate for another store, where you supply the two
  functions that read and add keys.
- **Can it be fixed?** No. The stores ASP.NET Core ships can only reach
  their store by [blocking](dotnet-runtime-differences.md#never-blocks). The replacements above are the
  lasting answer. They come with Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`,
  which every Isoplane ASP.NET Core project already references.

### `Html.Partial` and `Html.RenderPartial` {#html-partial}

[`Html.Partial`][Html.Partial] and [`Html.RenderPartial`][Html.RenderPartial]
wait synchronously for the partial view to render. They work when nothing in
the partial waits, and throw
[`PlatformNotSupportedException`][PlatformNotSupportedException] when the
partial awaits a query or a request.

```cshtml
@* Orders.cshtml *@
@Html.Partial("_RecentOrders")    @* throws, because the partial awaits *@

@* _RecentOrders.cshtml *@
@inject ShopDb Db
@foreach (var order in await Db.Orders.Take(5).ToListAsync())
{
    <li>@order.Number</li>
}
```

- **Used for:** rendering a partial view from a Razor view or page.
- **Workaround:** the [partial tag helper][PartialTagHelper],
  `<partial name="_RecentOrders" />`, or
  `@await Html.PartialAsync("_RecentOrders")`
  ([`PartialAsync`][Html.PartialAsync]). The build already reports the
  synchronous forms as [`MVC1000`][MVC1000].
- **Size of the change:** Small.
- **Can it be fixed?** No. Both can only render a partial that awaits by
  [blocking](dotnet-runtime-differences.md#never-blocks), so the asynchronous forms are the lasting answer.

### XML formatters over 30 KB {#xml-formatters}

With [`AddXmlSerializerFormatters`][AddXmlSerializerFormatters], MVC moves an
XML request or response larger than about 30 KB to a temporary file, which
fails with `DirectoryNotFoundException: /tmp/`.

- **Used for:** APIs that accept or return XML.
- **Workaround:** none through a setting that we have found. JSON through
  [`System.Text.Json`][System.Text.Json] has no such limit.
- **Size of the change:** None once this is fixed. Until then, large XML
  bodies do not work.
- **Can it be fixed?** Yes, with no change to your code. Isoplane can give
  ASP.NET Core somewhere in memory to buffer.

### Rate limiting middleware {#rate-limiting}

An application that calls [`UseRateLimiter`][UseRateLimiter] with a
fixed-window policy refuses to start: `initialization retains a timer`. The
limiter starts a timer when the application starts, and a timer cannot be
carried from initialization into an instance. Other policies have not been
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

- **Used for:** limiting how often a client may call an endpoint.
- **Workaround:** none inside the application. Count requests in a store that
  every instance shares, or limit them in front of the application.
- **Size of the change:** Large. Limiting moves out of the application, or onto
  a store the instances share.
- **Can it be fixed?** Partly. A change to Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`,
  could let this code start as written. The limiter would still count for one
  instance at a time, because its counters are in memory and
  [memory belongs to one instance](dotnet-runtime-differences.md#instances-come-and-go).

### `AddConsole`, `AddJsonConsole` and `ClearProviders` {#console-logging}

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

- **Used for:** these lines are in many `Program.cs` files, often copied from a
  template.
- **Workaround:** delete them. Isoplane already sends `ILogger` output to
  the terminal that runs the session, and the `Logging` section of
  `appsettings.json` filters it as usual. Serilog's
  [console sink][serilog-console] also works.
- **Size of the change:** Small.
- **Can it be fixed?** Yes, with no change to your code.
  Isoplane's ASP.NET Core adapter library, `Isoplane.AspNetCore`, can keep its
  own provider registered and leave the console provider out.

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

Each has a different way out:

- **Workaround:**
  - **Validation.** Run the check in the handler, or with a validator that has
    an asynchronous form. FluentValidation's
    [`MustAsync`][fluentvalidation-async] rules run with
    [`ValidateAsync`][fluentvalidation-async].
  - **Localized strings and other lookup data.** Load them asynchronously in
    an
    [`Initialization.Initializing`](reference/sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing.md)
    callback and serve them from memory. What initialization leaves in memory
    is copied into every instance.
  - **Signing keys.** Set [`Authority`][JwtBearerOptions.Authority] or
    [`MetadataAddress`][JwtBearerOptions.MetadataAddress] on the JWT bearer
    options. The handler then fetches and refreshes the keys asynchronously.
  - **Services.** Construct the object without connecting, and connect
    asynchronously the first time it is used.
- **Size of the change:** Moderate. The I/O moves to a place that can await
  it.
- **Can it be fixed?** No. These interfaces can only do I/O by
  blocking, and [blocking is ruled out](dotnet-runtime-differences.md#never-blocks). For one that is common
  enough, Isoplane can provide a replacement to call in its place, as it does
  for [Data Protection keys](#data-protection). That makes the workaround
  smaller; your code still changes.

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

- **Used for:** applications that keep Newtonsoft.Json for its converters or
  for compatibility with existing clients.
- **Workaround:** raise
  [`InputFormatterMemoryBufferThreshold`][InputFormatterMemoryBufferThreshold]
  and
  [`OutputFormatterMemoryBufferThreshold`][OutputFormatterMemoryBufferThreshold]
  to the largest body you expect. Bodies up to that size stay in memory, and a
  larger one fails as before.

  ```csharp
  const int LargestBody = 1024 * 1024;

  builder.Services.AddControllers().AddNewtonsoftJson(options =>
  {
      options.InputFormatterMemoryBufferThreshold = LargestBody;
      options.OutputFormatterMemoryBufferThreshold = LargestBody;
  });
  ```

- **Size of the change:** Small, and temporary. It is two settings.
- **Can it be fixed?** Yes, with no change to your code. Isoplane can give
  ASP.NET Core somewhere in memory to buffer.

## Configuration providers for remote stores

### Loading settings from a remote service {#remote-configuration}

A configuration provider loads through a synchronous method,
[`IConfigurationProvider.Load`][IConfigurationProvider.Load], so one that
calls a service has to block while the application starts. The application
then refuses to start, with
`unhandled System.PlatformNotSupportedException: Arg_PlatformNotSupported`.

```csharp
builder.Configuration.AddAzureKeyVault(
    new Uri("https://contoso.vault.azure.net/"), new DefaultAzureCredential());
```

We confirmed this with a provider written for the purpose. The providers
registered with [`AddAzureKeyVault`][AddAzureKeyVault] and
[`AddAzureAppConfiguration`][AddAzureAppConfiguration], and with
`AddSystemsManager` from the
[`Amazon.Extensions.Configuration.SystemsManager`][nuget-aws-ssm] package,
load the same way and are expected to fail likewise. They have not been run in
the sandbox.

- **Used for:** taking secrets and settings from a vault.
- **Workaround:** `Program.cs` may await before it builds the
  application, so fetch the values asynchronously there and add them with
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
- **Size of the change:** Moderate.
- **Can it be fixed?** No. A provider as it is can only load by
  [blocking](dotnet-runtime-differences.md#never-blocks). An adapter library from Isoplane could provide an
  asynchronous registration method for each provider, which would make the
  workaround one line.

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

- **Used for:** loading related entities when they are first touched.
- **Workaround:** load what the request needs as part of the query, with
  [`Include`][Include]:

  ```csharp
  var order = await db.Orders.Include(order => order.Customer).FirstAsync(order => order.Id == id);
  ```

  Or load a navigation explicitly before reading it, with
  [`LoadAsync`][ReferenceEntry.LoadAsync]:
  `await db.Entry(order).Reference(order => order.Customer).LoadAsync();`.
- **Size of the change:** Moderate. Every place that relies on a navigation
  being loaded on demand has to say what it needs.
- **Can it be fixed?** No. A property read cannot await, so the query would
  have to block, and [blocking is ruled out](dotnet-runtime-differences.md#never-blocks).

### `UseHiLo` keys with `Add` {#hilo}

With [`UseHiLo`][UseHiLo] key generation, [`DbContext.Add`][DbContext.Add] and
[`DbSet.Add`][DbSet.Add] fetch a block of key values from the database
synchronously when the current block runs out. The call throws
[`PlatformNotSupportedException`][PlatformNotSupportedException] on the first
`Add`, and again each time a block is used up, which is every tenth entity by
default. Between those points `Add` works, so the failure looks intermittent.

```csharp
// In OnModelCreating:
modelBuilder.Entity<Order>().Property(order => order.Id).UseHiLo();

// In a request:
db.Orders.Add(order);          // throws whenever it needs a new block of keys
await db.SaveChangesAsync();
```

- **Used for:** assigning keys before saving, with fewer round trips than a
  sequence.
- **Workaround:** `await db.Orders.AddAsync(order)`
  ([`AddAsync`][DbContext.AddAsync]), which fetches the block asynchronously.
  Or use identity columns, the Npgsql provider's default, which need no fetch.
- **Size of the change:** Small.
- **Can it be fixed?** No. `Add` can only fetch its keys by [blocking](dotnet-runtime-differences.md#never-blocks), so
  `AddAsync` is the lasting answer.

### `TransactionScope` {#transaction-scope}

A [`TransactionScope`][TransactionScope] commits when it is disposed, and it
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

- **Used for:** a transaction around several operations, without passing a
  transaction object between them.
- **Workaround:** use the provider's transaction, from
  [`BeginTransactionAsync`][BeginTransactionAsync], and commit it with
  [`CommitAsync`][CommitAsync].

  ```csharp
  await using var transaction = await db.Database.BeginTransactionAsync();
  db.Orders.Add(order);
  await db.SaveChangesAsync();
  await transaction.CommitAsync();
  ```

- **Size of the change:** Small for one context. Moderate when the scope
  spans several contexts or connections, which then have to share one
  transaction explicitly.
- **Can it be fixed?** Only in .NET itself, which has no asynchronous way to
  complete a `TransactionScope`.

## Database drivers

### Disposing a reader that has unread rows {#reader-dispose}

Disposing an [`NpgsqlDataReader`][NpgsqlDataReader] synchronously while rows
remain unread makes the driver read the rest synchronously, which throws
[`PlatformNotSupportedException`][PlatformNotSupportedException].

```csharp
using var reader = await command.ExecuteReaderAsync();
if (await reader.ReadAsync())
    return reader.GetString(0);   // rows remain unread, so disposing the reader throws
```

Other synchronous disposal works: a connection, a transaction, including one
that was never committed, and a reader that was read to the end.

- **Used for:** `using var reader = ...` in code that stops reading early.
- **Workaround:** `await using var reader = await command.ExecuteReaderAsync();`.
- **Size of the change:** Small.
- **Can it be fixed?** No. A synchronous dispose can only read the remaining
  rows by [blocking](dotnet-runtime-differences.md#never-blocks), so `await using` is the lasting answer.

### SQL Server {#sql-server}

[`Microsoft.Data.SqlClient`][nuget-sqlclient] has no implementation for this
platform: creating a [`SqlConnection`][SqlConnection] throws
`PlatformNotSupportedException: Microsoft.Data.SqlClient is not supported on this platform.`
Everything built on it fails with it, including
[`Microsoft.EntityFrameworkCore.SqlServer`][nuget-ef-sqlserver] and its
[`UseSqlServer`][UseSqlServer].

- **Used for:** SQL Server and Azure SQL.
- **Workaround:** none. PostgreSQL through Npgsql works; see
  [Connect PostgreSQL](connect-postgresql.md).
- **Size of the change:** Large: a different database.
- **Can it be fixed?** Only by a change to the driver. We also ran the driver's
  Unix implementation in the sandbox: it signs in synchronously inside
  [`OpenAsync`][DbConnection.OpenAsync], so it would need to open connections
  asynchronously throughout.

### MySQL {#mysql}

[`MySqlConnection.OpenAsync`][MySqlConnection.OpenAsync], from the
[`MySqlConnector`][MySqlConnector] package, fails with
`Unable to connect to any of the specified MySQL hosts`. The driver sets the
keep-alive option on its socket, the sandbox refuses that option, and the
driver reports the refusal as an unreachable host. Providers built on
`MySqlConnector`, such as
[`Pomelo.EntityFrameworkCore.MySql`][Pomelo], are expected to fail the same way
and have not been run.

- **Used for:** MySQL and MariaDB.
- **Workaround:** none.
- **Size of the change:** None once this is fixed. Until then, MySQL cannot
  be used.
- **Can it be fixed?** Probably, with no change to your code. Isoplane's runtime can
  accept the option. Whether the driver then works has not been established.

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

- **Used for:** local development databases, tests and small applications.
- **Workaround:** none. Use PostgreSQL in development as well. The
  `isoplane-webapi` template includes a compose file for it; see the
  [project templates](reference/project-templates.md).
- **Size of the change:** Large: a different database, in development as
  well as in production.
- **Can it be fixed?** No. Native code would run outside the sandbox, and
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

- **Used for:** MongoDB and compatible document databases.
- **Workaround:** none.
- **Size of the change:** Large: a different database.
- **Can it be fixed?** Only by a change to the driver, which would need to
  monitor its servers without threads of its own.

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
`Microsoft.Extensions.Caching.StackExchangeRedis`, the Redis output cache
store ([`AddStackExchangeRedisOutputCache`][AddStackExchangeRedisOutputCache]),
and
[`PersistKeysToStackExchangeRedis`][PersistKeysToStackExchangeRedis].

- **Used for:** a cache shared between instances, session state, and
  Data Protection keys.
- **Workaround:** none with this client.
  [`IDistributedCache`][IDistributedCache], session and
  [`HybridCache`][HybridCache] work over a cache that reaches its store
  through asynchronous calls; we ran them over PostgreSQL. For Data Protection
  keys, see [Data Protection key stores](#data-protection).
- **Size of the change:** Large: a different store for the cache.
- **Can it be fixed?** Only by a change to the client library. Why the
  thread-pool configuration stalls is not yet known.

## Observability

### Exporting telemetry over OTLP {#opentelemetry}

An application that adds OpenTelemetry tracing with the OTLP exporter, from
the
[`OpenTelemetry.Exporter.OpenTelemetryProtocol`][otlp-exporter] package,
refuses to start: `initialization retains a timer`. The exporter collects
telemetry and sends it in batches from the background.

```csharp
builder.Services.AddOpenTelemetry()
    .WithTracing(tracing => tracing
        .AddAspNetCoreInstrumentation()
        .AddOtlpExporter());   // the application refuses to start
```

- **Used for:** sending traces, metrics and logs to a collector.
- **Workaround:** none that we have run.
- **Size of the change:** Not known. Nothing works today.
- **Can it be fixed?** Not known yet. Sending in batches after the response is
  [work between requests](dotnet-runtime-differences.md#work-belongs-to-a-request),
  so an exporter would have to send within the request instead.

## Packages that work {#works}

These ran in the sandbox as they would anywhere else. Each entry says what we
exercised, which is not everything the package does.

- **ASP.NET Core 10.0.** Minimal APIs, MVC controllers and Razor views, JSON
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
- **[`Dapper`][dapper] 2.1.89** with Npgsql. `QueryAsync`, `QueryFirstAsync` over a
  large result, and `ExecuteAsync`.
- **`Microsoft.Extensions.Caching.Hybrid` 10.10.0.** [`GetOrCreateAsync`][HybridCache.GetOrCreateAsync],
  with and without a distributed second level.
- **`Microsoft.Extensions.Http.Resilience` 10.10.0.** A request through
  [`AddStandardResilienceHandler`][AddStandardResilienceHandler].
- **[`Serilog.AspNetCore`][serilog-aspnetcore] 10.0.0.** `UseSerilog` with the
  [console sink][serilog-console].
- **[`FluentValidation`][fluentvalidation] 12.2.0.** Synchronous and
  asynchronous rules through [`ValidateAsync`][fluentvalidation-async].
- **[`MediatR`][mediatr] 12.5.0.** `Send` to a handler that queries a database.
- **[`AutoMapper`][automapper] 14.0.0.** Mapping an entity to a record.
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
- **`Microsoft.Identity.Web`.**
- **`MailKit`.** It builds into an application package. Sending mail has not
  been run.
- **`Grpc.Net.Client`**, and Azure Cosmos DB.
- **Job schedulers and message consumers**, such as Hangfire, Quartz.NET and
  MassTransit. They rely on
  [work between requests](dotnet-runtime-differences.md#work-belongs-to-a-request).
- **Rate limiting policies other than fixed window**, and OpenTelemetry
  exporters other than OTLP.

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
[AddNewtonsoftJson]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.newtonsoftjsonmvcbuilderextensions.addnewtonsoftjson
[AddRateLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.builder.ratelimiterservicecollectionextensions.addratelimiter
[AddSession]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.sessionservicecollectionextensions.addsession
[AddSessionStateTempDataProvider]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.mvcviewfeaturesmvcbuilderextensions.addsessionstatetempdataprovider
[AddStackExchangeRedisCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.stackexchangerediscacheservicecollectionextensions.addstackexchangerediscache
[AddStackExchangeRedisOutputCache]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.stackexchangeredisoutputcacheservicecollectionextensions.addstackexchangeredisoutputcache
[AddStandardResilienceHandler]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.resiliencehttpclientbuilderextensions.addstandardresiliencehandler
[AddXmlSerializerFormatters]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.dependencyinjection.mvcxmlmvcbuilderextensions.addxmlserializerformatters
[AllowSynchronousIO]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.server.kestrel.core.kestrelserveroptions.allowsynchronousio
[AsFileSection]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.webutilities.multipartsectionconverterextensions.asfilesection
[automapper]: https://docs.automapper.io/
[BackgroundService]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.hosting.backgroundservice
[BeginTransactionAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.infrastructure.databasefacade.begintransactionasync
[ClearProviders]: https://learn.microsoft.com/dotnet/api/microsoft.extensions.logging.loggingbuilderextensions.clearproviders
[CommitAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.storage.idbcontexttransaction.commitasync
[Controller.TempData]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.controller.tempdata
[CryptographicException]: https://learn.microsoft.com/dotnet/api/system.security.cryptography.cryptographicexception
[dapper]: https://www.learndapper.com/
[DbConnection.OpenAsync]: https://learn.microsoft.com/dotnet/api/system.data.common.dbconnection.openasync
[DbContext.Add]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbcontext.add
[DbContext.AddAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbcontext.addasync
[DbContext.SaveChanges]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbcontext.savechanges
[DbSet.Add]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.dbset-1.add
[DllNotFoundException]: https://learn.microsoft.com/dotnet/api/system.dllnotfoundexception
[EnableBuffering]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequestrewindextensions.enablebuffering
[fluentvalidation]: https://docs.fluentvalidation.net/
[fluentvalidation-async]: https://docs.fluentvalidation.net/en/latest/async.html
[FormOptions]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions
[FormOptions.MemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions.memorybufferthreshold
[FormOptions.MultipartBodyLengthLimit]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.features.formoptions.multipartbodylengthlimit
[Html.Partial]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.rendering.htmlhelperpartialextensions.partial
[Html.PartialAsync]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.rendering.htmlhelperpartialextensions.partialasync
[Html.RenderPartial]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.rendering.htmlhelperpartialextensions.renderpartial
[HttpClient]: https://learn.microsoft.com/dotnet/api/system.net.http.httpclient
[HttpContext.Session]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httpcontext.session
[HttpRequest.Body]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequest.body
[HttpRequest.Form]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequest.form
[HttpRequest.ReadFormAsync]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httprequest.readformasync
[HttpResponse.Body]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.http.httpresponse.body
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
[mediatr]: https://github.com/jbogard/MediatR
[MemoryStream]: https://learn.microsoft.com/dotnet/api/system.io.memorystream
[Microsoft.Data.Sqlite]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlite
[MongoDB.Driver]: https://www.mongodb.com/docs/drivers/csharp/current/
[MultipartReader]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.webutilities.multipartreader
[MVC1000]: https://learn.microsoft.com/aspnet/core/diagnostics/mvc1000
[MySqlConnection.OpenAsync]: https://mysqlconnector.net/api/mysqlconnector/mysqlconnection/openasync/
[MySqlConnector]: https://mysqlconnector.net/
[NetworkStream]: https://learn.microsoft.com/dotnet/api/system.net.sockets.networkstream
[NotSupportedException]: https://learn.microsoft.com/dotnet/api/system.notsupportedexception
[NpgsqlDataReader]: https://www.npgsql.org/doc/api/Npgsql.NpgsqlDataReader.html
[nuget-aws-ssm]: https://www.nuget.org/packages/Amazon.Extensions.Configuration.SystemsManager
[nuget-dp-ef]: https://www.nuget.org/packages/Microsoft.AspNetCore.DataProtection.EntityFrameworkCore
[nuget-ef-proxies]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.Proxies
[nuget-ef-sqlite]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.Sqlite
[nuget-ef-sqlserver]: https://www.nuget.org/packages/Microsoft.EntityFrameworkCore.SqlServer
[nuget-newtonsoft]: https://www.nuget.org/packages/Microsoft.AspNetCore.Mvc.NewtonsoftJson
[nuget-sqlclient]: https://www.nuget.org/packages/Microsoft.Data.SqlClient
[otlp-exporter]: https://github.com/open-telemetry/opentelemetry-dotnet/blob/main/src/OpenTelemetry.Exporter.OpenTelemetryProtocol/README.md
[OutputFormatterMemoryBufferThreshold]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.mvcnewtonsoftjsonoptions.outputformattermemorybufferthreshold
[PartialTagHelper]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.mvc.taghelpers.partialtaghelper
[PersistKeysToDbContext]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.entityframeworkcoredataprotectionextensions.persistkeystodbcontext
[PersistKeysToFileSystem]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.dataprotectionbuilderextensions.persistkeystofilesystem
[PersistKeysToStackExchangeRedis]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.dataprotection.stackexchangeredisdataprotectionbuilderextensions.persistkeystostackexchangeredis
[PlatformNotSupportedException]: https://learn.microsoft.com/dotnet/api/system.platformnotsupportedexception
[Pomelo]: https://github.com/PomeloFoundation/Pomelo.EntityFrameworkCore.MySql
[ReferenceEntry.LoadAsync]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.changetracking.referenceentry.loadasync
[se-redis-basics]: https://stackexchange.github.io/StackExchange.Redis/Basics
[se-redis-configuration]: https://stackexchange.github.io/StackExchange.Redis/Configuration
[SemaphoreSlim.Wait]: https://learn.microsoft.com/dotnet/api/system.threading.semaphoreslim.wait
[serilog-aspnetcore]: https://github.com/serilog/serilog-aspnetcore
[serilog-console]: https://github.com/serilog/serilog-sinks-console
[Socket]: https://learn.microsoft.com/dotnet/api/system.net.sockets.socket
[SqlConnection]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlclient.sqlconnection
[SqliteConnection]: https://learn.microsoft.com/dotnet/api/microsoft.data.sqlite.sqliteconnection
[StackExchange.Redis]: https://stackexchange.github.io/StackExchange.Redis/
[Stream.CopyToAsync]: https://learn.microsoft.com/dotnet/api/system.io.stream.copytoasync
[Stream.Read]: https://learn.microsoft.com/dotnet/api/system.io.stream.read
[Stream.ReadAsync]: https://learn.microsoft.com/dotnet/api/system.io.stream.readasync
[Stream.Write]: https://learn.microsoft.com/dotnet/api/system.io.stream.write
[Stream.WriteAsync]: https://learn.microsoft.com/dotnet/api/system.io.stream.writeasync
[swashbuckle]: https://github.com/domaindrivendev/Swashbuckle.AspNetCore
[System.Text.Json]: https://learn.microsoft.com/dotnet/api/system.text.json
[Task]: https://learn.microsoft.com/dotnet/api/system.threading.tasks.task
[Task.Result]: https://learn.microsoft.com/dotnet/api/system.threading.tasks.task-1.result
[Thread.Sleep]: https://learn.microsoft.com/dotnet/api/system.threading.thread.sleep
[Thread.Start]: https://learn.microsoft.com/dotnet/api/system.threading.thread.start
[TokenValidationParameters]: https://learn.microsoft.com/dotnet/api/microsoft.identitymodel.tokens.tokenvalidationparameters
[TransactionInDoubtException]: https://learn.microsoft.com/dotnet/api/system.transactions.transactionindoubtexception
[TransactionScope]: https://learn.microsoft.com/dotnet/api/system.transactions.transactionscope
[UseHiLo]: https://www.npgsql.org/efcore/api/Microsoft.EntityFrameworkCore.NpgsqlPropertyBuilderExtensions.html
[UseLazyLoadingProxies]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.proxiesextensions.uselazyloadingproxies
[UseRateLimiter]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.builder.ratelimiterapplicationbuilderextensions.useratelimiter
[UseSession]: https://learn.microsoft.com/dotnet/api/microsoft.aspnetcore.builder.sessionmiddlewareextensions.usesession
[UseSqlite]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.sqlitedbcontextoptionsbuilderextensions.usesqlite
[UseSqlServer]: https://learn.microsoft.com/dotnet/api/microsoft.entityframeworkcore.sqlserverdbcontextoptionsextensions.usesqlserver
[ValidationAttribute.IsValid]: https://learn.microsoft.com/dotnet/api/system.componentmodel.dataannotations.validationattribute.isvalid
[XmlSerializer.Serialize]: https://learn.microsoft.com/dotnet/api/system.xml.serialization.xmlserializer.serialize
