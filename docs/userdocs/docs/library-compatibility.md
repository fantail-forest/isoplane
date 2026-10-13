---
title: Library compatibility
---

Most NuGet packages run on Isoplane unchanged. The ones that do not fail for a
handful of reasons, all of which follow from
[how .NET differs on Isoplane](dotnet-runtime-differences.md): something waits
synchronously, starts a thread or a timer, writes a file, or loads a native
library.

This page lists the incompatibilities we know of, by package. Each entry says
what fails, what you can do about it today, how much of your code has to
change, and whether the incompatibility can be fixed and, if it can, what
that requires. [Packages that work](#works) and
[packages not checked yet](#not-checked) are at the end.

Everything here was run in the sandbox with SDK 0.1.0-preview.4, unless its
entry says otherwise.

## At a glance {#summary}

| Package | API or feature | Change to your code | Can it be fixed? |
| --- | --- | --- | --- |
| Any | [Synchronous calls that reach the network](#synchronous-io) | Small: call the asynchronous method | No: these calls have to block |
| ASP.NET Core | [Session over a distributed cache, and TempData kept in it](#session) | Small: add one middleware | Yes, in Isoplane |
| ASP.NET Core | [`Request.Form`](#request-form) | Small: use `ReadFormAsync` | Nothing to fix |
| ASP.NET Core | [File uploads over 64 KB](#uploads) | Small: set one option | Yes, in Isoplane |
| ASP.NET Core | [`Request.EnableBuffering()` over 30 KB](#enable-buffering) | Small: pass one argument | Yes, in Isoplane |
| ASP.NET Core | [Synchronous reads and writes of the request and response body](#synchronous-body) | Small: call the asynchronous method | Nothing to fix |
| ASP.NET Core | [Data Protection key stores](#data-protection) | Small: use Isoplane's replacement | Fixed, by that replacement |
| ASP.NET Core | [`Html.Partial` and `Html.RenderPartial`](#html-partial) | Small: use `<partial>` | Nothing to fix |
| ASP.NET Core | [XML formatters over 30 KB](#xml-formatters) | No workaround | Yes, in Isoplane |
| ASP.NET Core | [Rate limiting middleware](#rate-limiting) | Large: limit requests elsewhere | Partly, in Isoplane |
| ASP.NET Core | [`AddConsole`, `AddJsonConsole` and `ClearProviders`](#console-logging) | Small: delete the calls | Yes, in Isoplane |
| ASP.NET Core | [Validation attributes, localizers, token callbacks and service factories that do I/O](#synchronous-extension-points) | Moderate: move the I/O elsewhere | No: these have to block |
| ASP.NET Core | [WebSockets, SignalR's WebSocket transport, Blazor Server](#connections-and-background-work) | No workaround | Needs instances that stay connected |
| ASP.NET Core | [gRPC over HTTP/2](#connections-and-background-work) | Moderate: use gRPC-Web | Not known yet |
| ASP.NET Core | [`BackgroundService` and other work between requests](#connections-and-background-work) | Large: do the work during a request | Needs instances that stay running |
| Newtonsoft.Json for MVC | [Bodies over 30 KB](#newtonsoft-json) | Small: set two options | Yes, in Isoplane |
| Configuration providers | [Loading settings from Azure Key Vault and other remote services](#remote-configuration) | Moderate: fetch them before `Build()` | Yes, by a replacement from Isoplane |
| Entity Framework Core | [Lazy loading](#lazy-loading) | Moderate: load with `Include` | No: it has to block |
| Entity Framework Core | [`UseHiLo` keys with `Add`](#hilo) | Small: use `AddAsync` | Nothing to fix |
| Entity Framework Core | [`TransactionScope`](#transaction-scope) | Small: use a database transaction | Only in .NET itself |
| Npgsql | [Disposing a reader that has unread rows](#reader-dispose) | Small: use `await using` | Nothing to fix |
| SqlClient | [SQL Server, including its Entity Framework Core provider](#sql-server) | Large: use another database | Only in the driver |
| MySqlConnector | [MySQL](#mysql) | No workaround | Probably, in Isoplane |
| Sqlite | [SQLite, including its Entity Framework Core provider](#sqlite) | Large: use another database | No: it needs native code |
| MongoDB.Driver | [MongoDB](#mongodb) | Large: use another database | Only in the driver |
| StackExchange.Redis | [Redis, including the Redis distributed cache](#redis) | Large: use another cache store | Only in the client library |
| OpenTelemetry | [Exporting telemetry over OTLP](#opentelemetry) | No workaround | Not known yet |

The sections below give each package's full name.

<details>
<summary>How the changes are sized</summary>

- **Small** is a one-for-one replacement: the asynchronous form of the same
  call, a setting, or a method Isoplane provides.
- **Moderate** is some restructuring, or a few lines of your own.
- **Large** is a different library or a different approach.

"Yes, in Isoplane" means the SDK or runtime can be changed so that your code
works as it was written, and any workaround can then come out. It describes
what is possible, not a schedule.

</details>

## Recognize a failure {#symptoms}

Exception messages from the runtime are short identifiers, not sentences, so
read the stack trace to see which call failed.

| What you see | What it means |
| --- | --- |
| `PlatformNotSupportedException: Arg_PlatformNotSupported`, with `Task.InternalWait`, `SemaphoreSlim.Wait`, `Thread.Sleep`, `Socket` or `NetworkStream` near the top of the stack | Something waited synchronously. Start with [synchronous calls](#synchronous-io). |
| `PlatformNotSupportedException: net_wasi_sync_name_resolution` | A connection was opened synchronously. Same cause. |
| `PlatformNotSupportedException: Arg_PlatformNotSupported`, with `Thread.Start` near the top of the stack | A library started a thread. See [MongoDB](#mongodb) and [Redis](#redis). |
| `DirectoryNotFoundException: /tmp/` | ASP.NET Core tried to move a large request or response to a temporary file. See [uploads](#uploads), [`EnableBuffering`](#enable-buffering), [Newtonsoft.Json](#newtonsoft-json) and [XML formatters](#xml-formatters). |
| `IOException: Read-only file system` | Something tried to write a file. The [file system is read-only](dotnet-runtime-differences.md#the-machine-underneath). |
| `DllNotFoundException` | A package called into a native library. See [SQLite](#sqlite). |
| The application refuses to start with `initialization retains a timer` | Something registered at startup started a timer. See [rate limiting](#rate-limiting) and [OpenTelemetry](#opentelemetry). |
| The application refuses to start with `unhandled System.PlatformNotSupportedException` and no stack trace | A call made while the application starts failed. The known ones are [console logging](#console-logging) and [remote configuration](#remote-configuration). |
| No error, but session values or TempData are missing on the next request | The session was loaded synchronously. See [session](#session). |

## Any library

### Synchronous calls that reach the network {#synchronous-io}

A synchronous method that has to wait for the network throws
`PlatformNotSupportedException`. That covers `DbConnection.Open`,
`DbCommand.ExecuteReader`, `DbContext.SaveChanges`, a LINQ `ToList` on an Entity
Framework Core query, `HttpClient.Send`, and waiting on an unfinished task with
`.Result` or `.GetAwaiter().GetResult()`.
[Waiting is asynchronous](dotnet-runtime-differences.md#waiting-is-asynchronous)
explains why.

- **Used for:** older code, and code written to be called from a synchronous
  method.
- **What to do today:** call the asynchronous method and await it: `OpenAsync`,
  `ExecuteReaderAsync`, `SaveChangesAsync`, `ToListAsync`, `SendAsync`.
- **Size of the change:** Small. Each call has an asynchronous twin, though the methods
  that contain it have to become asynchronous too.
- **Can it be fixed?** No. An application in the sandbox cannot block, and these calls
  have to.

The rest of this page is about the cases where your own code is asynchronous
and the blocking call is inside a library, or behind an interface that has no
asynchronous form.

## ASP.NET Core

These ship with the Isoplane SDK as part of ASP.NET Core, so they need no
package reference.

### Session over a distributed cache {#session}

With `AddSession` and an `IDistributedCache` that reaches a database or another
service, the first read or write of `HttpContext.Session` in a request loads the
session synchronously. The load fails, ASP.NET Core logs
`Session cache read exception`, and the request carries on with an empty
session. Reads return nothing and writes are not saved. No exception reaches
your code.

TempData is lost the same way when it is kept in the session with
`AddSessionStateTempDataProvider`.

- **Used for:** sign-in flows, shopping carts, multi-step forms, and messages
  shown after a redirect.
- **What to do today:** load the session asynchronously before anything uses it, in a
  middleware placed directly after `UseSession`:

  ```csharp
  app.UseSession();
  app.Use(async (context, next) =>
  {
      await context.Session.LoadAsync(context.RequestAborted);
      await next();
  });
  ```

- **Size of the change:** Small. One middleware, added once.
- **Can it be fixed?** Yes, in Isoplane. The SDK can report an unloaded session as
  an error in place of losing data, and provide this middleware.

We ran this with a PostgreSQL-backed cache. The Redis cache does not run at
all; see [Redis](#redis).

### `Request.Form` {#request-form}

Reading `HttpRequest.Form` before the form has been read asynchronously throws
`PlatformNotSupportedException`, whatever the size of the body.

- **Used for:** reading posted fields and files directly from the request.
- **What to do today:** `var form = await Request.ReadFormAsync();`. A minimal API
  handler that takes a form value or an `IFormFile` as a parameter already
  reads asynchronously.
- **Size of the change:** Small.
- **Can it be fixed?** There is nothing to fix: the asynchronous call does the
  same job.

### File uploads over 64 KB {#uploads}

ASP.NET Core keeps the first 64 KB of an uploaded file in memory and moves a
larger one to a temporary file. There is nowhere to write one, so
`ReadFormAsync` throws `DirectoryNotFoundException: /tmp/`. A minimal API
handler that takes an `IFormFile` parameter answers 400 with an empty body
instead.

- **Used for:** any multipart upload: images, documents, imports.
- **What to do today:** keep uploads in memory.

  ```csharp
  using Microsoft.AspNetCore.Http.Features;

  builder.Services.Configure<FormOptions>(options => options.MemoryBufferThreshold = int.MaxValue);
  ```

  An upload held in memory counts toward the instance's
  [memory limit](execution-limits.md#memory). ASP.NET Core still refuses a form
  larger than `FormOptions.MultipartBodyLengthLimit`.
- **Size of the change:** Small, and temporary. It is one setting, which can come
  out once this is fixed.
- **Can it be fixed?** Yes, in Isoplane, with no change to your code. The SDK can
  make this the default.

Large fields of an ordinary URL-encoded form are not affected.

### `Request.EnableBuffering()` over 30 KB {#enable-buffering}

`EnableBuffering` lets a request body be read more than once. It keeps 30 KB in
memory and moves a larger body to a temporary file, which fails with
`DirectoryNotFoundException: /tmp/` when the body is read.

- **Used for:** verifying a webhook signature before binding the body, and
  logging request bodies.
- **What to do today:** pass a threshold, so the body stays in memory:
  `Request.EnableBuffering(bufferThreshold: int.MaxValue)`. The body then counts
  toward the instance's [memory limit](execution-limits.md#memory).
- **Size of the change:** Small, and temporary. It is one argument.
- **Can it be fixed?** Yes, in Isoplane, with no change to your code.

### Synchronous reads and writes of the request and response body {#synchronous-body}

`Request.Body.Read` and `Response.Body.Write` throw `NotSupportedException`,
with a message that says to use the asynchronous form. ASP.NET Core's own
server refuses these calls by default as well.

- **Used for:** serializers and writers that take a `Stream` and write to it
  synchronously, such as `XmlSerializer.Serialize(Response.Body, value)`.
- **What to do today:** use `ReadAsync` and `WriteAsync`, or the library's
  asynchronous method. Where a library has none, have it write to a
  `MemoryStream` and copy that to the response with `CopyToAsync`.
- **Size of the change:** Small.
- **Can it be fixed?** There is nothing to fix: the asynchronous methods do the
  same job.

### Data Protection key stores {#data-protection}

Data Protection reads and writes its keys through a synchronous interface, so a
store that keeps them in a database or another service cannot work.
`PersistKeysToDbContext` fails when the keys are first read: ASP.NET Core logs
`An error occurred while reading the key ring`, and protecting data then throws
`CryptographicException`. `PersistKeysToFileSystem` fails with
`Read-only file system`.

- **Used for:** keeping authentication cookies and antiforgery tokens valid
  after a new build.
- **What to do today:** Isoplane provides replacements, described in
  [Keep Data Protection keys across builds](dotnet-runtime-differences.md#keep-data-protection-keys-across-builds).
  `UseKeysFromDbContext` takes the place of `PersistKeysToDbContext` and uses
  the same table. `UseConfiguredKeys` derives the keys from a secret in the
  application's settings.
  [`UseKeysFrom`](reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/DataProtectionKeys/UseKeysFrom.md)
  serves any other store.
- **Size of the change:** Small for Entity Framework Core, where one call
  replaces another. Moderate for another store, where you supply the two
  functions that read and add keys.
- **Can it be fixed?** It has been, for Entity Framework Core and for configured
  secrets, by the replacements above. The stores ASP.NET Core ships cannot
  work, because they have to block.

### `Html.Partial` and `Html.RenderPartial` {#html-partial}

Both wait synchronously for the partial view to render. They work when nothing
in the partial waits, and throw `PlatformNotSupportedException` when the
partial awaits a query or a request.

- **Used for:** rendering a partial view from a Razor view or page.
- **What to do today:** `<partial name="_Name" />`, or
  `@await Html.PartialAsync("_Name")`. The build already reports the
  synchronous forms as `MVC1000`.
- **Size of the change:** Small.
- **Can it be fixed?** There is nothing to fix: the asynchronous forms do the
  same job.

### XML formatters over 30 KB {#xml-formatters}

With `AddXmlSerializerFormatters`, MVC moves an XML request or response larger
than about 30 KB to a temporary file, which fails with
`DirectoryNotFoundException: /tmp/`.

- **Used for:** APIs that accept or return XML.
- **What to do today:** none through a setting that we have found. JSON through
  `System.Text.Json` has no such limit.
- **Size of the change:** None once this is fixed. Until then, large XML
  bodies do not work.
- **Can it be fixed?** Yes, in Isoplane, with no change to your code.

### Rate limiting middleware {#rate-limiting}

An application that calls `UseRateLimiter` with a fixed-window policy refuses to
start: `initialization retains a timer`. The limiter starts a timer when the
application starts, and a timer cannot be carried from initialization into an
instance. Other policies have not been checked.

- **Used for:** limiting how often a client may call an endpoint.
- **What to do today:** none inside the application. Count requests in a store that
  every instance shares, or limit them in front of the application.
- **Size of the change:** Large. Limiting moves out of the application, or onto
  a store the instances share.
- **Can it be fixed?** Partly, in Isoplane. The SDK can let the application
  start. The limiter would still count for one instance at a time, because its
  counters are in memory and
  [memory belongs to one instance](dotnet-runtime-differences.md#instances-come-and-go).

### `AddConsole`, `AddJsonConsole` and `ClearProviders` {#console-logging}

An application that calls `builder.Logging.AddConsole()` or `AddJsonConsole()`
refuses to start, with
`unhandled System.PlatformNotSupportedException: Arg_PlatformNotSupported` and
no stack trace: the console logger starts a thread.

`builder.Logging.ClearProviders()` on its own lets the application start, but
removes the provider Isoplane registers, and `ILogger` output is no longer
shown.

- **Used for:** these lines are in many `Program.cs` files, often copied from a
  template.
- **What to do today:** delete them. Isoplane already sends `ILogger` output to the
  terminal that runs the session, and the `Logging` section of `appsettings.json`
  filters it as usual. Serilog's console sink also works.
- **Size of the change:** Small.
- **Can it be fixed?** Yes, in Isoplane, with no change to your code. The SDK can
  keep its provider registered and leave the console provider out.

### Validation attributes, localizers, token callbacks and service factories that do I/O {#synchronous-extension-points}

Some extension points are synchronous by definition, so an implementation that
queries a database or calls a service has to block, and throws
`PlatformNotSupportedException`:

- `ValidationAttribute.IsValid` and `IValidatableObject.Validate`, for example
  a "this name is not taken" check.
- `IStringLocalizer`, when the strings come from a database.
- `TokenValidationParameters` callbacks such as `IssuerSigningKeyResolver`,
  when they fetch signing keys.
- Constructors and dependency injection factories that connect when the object
  is created.

Each has a different way out:

- **What to do today:**
  - **Validation.** Run the check in the handler, or with a validator that has
    an asynchronous form. FluentValidation's `MustAsync` rules run with
    `ValidateAsync`.
  - **Localized strings and other lookup data.** Load them asynchronously in
    an
    [`Initialization.Initializing`](reference/sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing.md)
    callback and serve them from memory. What initialization leaves in memory
    is copied into every instance.
  - **Signing keys.** Set `Authority` or `MetadataAddress` on the JWT bearer
    options. The handler then fetches and refreshes the keys asynchronously.
  - **Services.** Construct the object without connecting, and connect
    asynchronously the first time it is used.
- **Size of the change:** Moderate. The I/O moves to a place that can await
  it.
- **Can it be fixed?** Not in general: these interfaces have to block. Where one
  is common enough, Isoplane can provide a replacement, as it does for
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
- **A `BackgroundService` that keeps running**, a recurring timer or a queue
  consumer has nothing to run on between requests. Doing that work while a
  request is handled is a large change. See
  [Work belongs to a request](dotnet-runtime-differences.md#work-belongs-to-a-request).

## `Microsoft.AspNetCore.Mvc.NewtonsoftJson`

### Bodies over 30 KB {#newtonsoft-json}

With `AddNewtonsoftJson`, MVC moves a JSON request or response larger than
about 30 KB to a temporary file, which fails with
`DirectoryNotFoundException: /tmp/`.

- **Used for:** applications that keep Newtonsoft.Json for its converters or
  for compatibility with existing clients.
- **What to do today:** raise both thresholds, so bodies stay in memory.

  ```csharp
  builder.Services.AddControllers().AddNewtonsoftJson(options =>
  {
      options.InputFormatterMemoryBufferThreshold = int.MaxValue;
      options.OutputFormatterMemoryBufferThreshold = int.MaxValue;
  });
  ```

- **Size of the change:** Small, and temporary. It is two settings.
- **Can it be fixed?** Yes, in Isoplane, with no change to your code.

## Configuration providers for remote stores

### Loading settings from a remote service {#remote-configuration}

A configuration provider loads through a synchronous method, so one that calls
a service has to block while the application starts. The application then
refuses to start, with
`unhandled System.PlatformNotSupportedException: Arg_PlatformNotSupported`.

We confirmed this with a provider written for the purpose. The providers for
Azure Key Vault (`AddAzureKeyVault`), Azure App Configuration and AWS Systems
Manager load the same way and are expected to fail likewise. They have not been
run in the sandbox.

- **Used for:** taking secrets and settings from a vault.
- **What to do today:** `Program.cs` may await before it builds the application, so
  fetch the values asynchronously there and add them as an in-memory source:

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
- **Can it be fixed?** Yes, by a replacement from Isoplane: an asynchronous
  registration method for each provider would make this one line. The
  providers as they are cannot work, because they have to block.

{/* TODO: say how settings and secrets reach a deployed application once that exists; it is expected to make this entry unnecessary for most applications. */}

## Entity Framework Core

Entity Framework Core works through its asynchronous methods. We ran it with
the Npgsql provider; see [Packages that work](#works).

### Lazy loading {#lazy-loading}

With `UseLazyLoadingProxies` from `Microsoft.EntityFrameworkCore.Proxies`, or an injected `ILazyLoader`, reading a navigation
property runs a query synchronously and throws
`PlatformNotSupportedException`. A navigation whose target the context already
tracks is read from memory, so the same line can work for one row and fail for
another.

- **Used for:** loading related entities when they are first touched.
- **What to do today:** load what the request needs as part of the query, with
  `Include`:

  ```csharp
  var book = await db.Books.Include(book => book.Author).FirstAsync();
  ```

  Or load a navigation explicitly before reading it:
  `await db.Entry(book).Reference(book => book.Author).LoadAsync();`.
- **Size of the change:** Moderate. Every place that relies on a navigation
  being loaded on demand has to say what it needs.
- **Can it be fixed?** No. Reading a property cannot wait, so it would have to
  block.

### `UseHiLo` keys with `Add` {#hilo}

With HiLo key generation, `DbContext.Add` and `DbSet.Add` fetch a block of key
values from the database synchronously when the current block runs out. The
call throws `PlatformNotSupportedException` on the first `Add`, and again each
time a block is used up, which is every tenth entity by default. Between those
points `Add` works, so the failure looks intermittent.

- **Used for:** assigning keys before `SaveChanges`, with fewer round trips
  than a sequence.
- **What to do today:** `await db.AddAsync(entity)`, which fetches the block
  asynchronously. Or use identity columns, the Npgsql provider's default, which
  need no fetch.
- **Size of the change:** Small.
- **Can it be fixed?** There is nothing to fix: `AddAsync` does the same job.

### `TransactionScope` {#transaction-scope}

A `TransactionScope` commits when it is disposed, and it can only be disposed
synchronously. With work done on a connection inside the scope, disposing it
throws `TransactionInDoubtException`.

- **Used for:** a transaction around several operations, without passing a
  transaction object between them.
- **What to do today:** use the provider's transaction and commit it asynchronously.

  ```csharp
  await using var transaction = await db.Database.BeginTransactionAsync();
  // ... the work ...
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

Disposing an `NpgsqlDataReader` synchronously while rows remain unread makes
the driver read the rest synchronously, which throws
`PlatformNotSupportedException`.

Other synchronous disposal works: a connection, a transaction, including one
that was never committed, and a reader that was read to the end.

- **Used for:** `using var reader = ...` in code that stops reading early.
- **What to do today:** `await using var reader = await command.ExecuteReaderAsync();`.
- **Size of the change:** Small.
- **Can it be fixed?** There is nothing to fix: `await using` does the same job.

### SQL Server {#sql-server}

`Microsoft.Data.SqlClient` has no implementation for this platform: creating a
`SqlConnection` throws
`PlatformNotSupportedException: Microsoft.Data.SqlClient is not supported on this platform.`
Everything built on it fails with it, including
`Microsoft.EntityFrameworkCore.SqlServer`.

- **Used for:** SQL Server and Azure SQL.
- **What to do today:** none. PostgreSQL through Npgsql works; see
  [Connect PostgreSQL](connect-postgresql.md).
- **Size of the change:** Large: a different database.
- **Can it be fixed?** Only by a change to the driver. We also ran the driver's
  Unix implementation in the sandbox: it signs in synchronously inside
  `OpenAsync`, so it would need to open connections asynchronously
  throughout.

### MySQL {#mysql}

`MySqlConnection.OpenAsync` fails with
`Unable to connect to any of the specified MySQL hosts`. The driver sets the
keep-alive option on its socket, the sandbox refuses that option, and the
driver reports the refusal as an unreachable host. Providers built on
`MySqlConnector`, such as `Pomelo.EntityFrameworkCore.MySql`, are expected to
fail the same way and have not been run.

- **Used for:** MySQL and MariaDB.
- **What to do today:** none.
- **Size of the change:** None once this is fixed. Until then, MySQL cannot
  be used.
- **Can it be fixed?** Probably, in Isoplane. The runtime can accept the option.
  Whether the driver then works has not been established.

### SQLite {#sqlite}

`Microsoft.Data.Sqlite` calls a native SQLite library, which the sandbox does
not load: opening a connection throws `DllNotFoundException: e_sqlite3`. That
includes in-memory databases, and `Microsoft.EntityFrameworkCore.Sqlite`,
which is built on it.

- **Used for:** local development databases, tests and small applications.
- **What to do today:** none. Use PostgreSQL in development as well. The
  `isoplane-webapi` template includes a compose file for it; see the
  [project templates](reference/project-templates.md).
- **Size of the change:** Large: a different database, in development as
  well as in production.
- **Can it be fixed?** No. It needs native code, which cannot run in the sandbox.

### MongoDB {#mongodb}

`MongoDB.Driver` starts a thread for each server it monitors. The first
operation throws `PlatformNotSupportedException` from `Thread.Start`.

- **Used for:** MongoDB and compatible document databases.
- **What to do today:** none.
- **Size of the change:** Large: a different database.
- **Can it be fixed?** Only by a change to the driver, which would need to
  monitor its servers without threads of its own.

## Caching

### Redis {#redis}

`StackExchange.Redis` starts threads for its connections, so
`ConnectionMultiplexer.Connect` and `ConnectAsync` throw
`PlatformNotSupportedException` from `Thread.Start`.

Configuring the client with `SocketManager.ThreadPool` avoids the threads but
does not help: the connection opens, the handshake never completes, and
`ConnectAsync` fails when its timeout passes. With the Redis cache registered
that way as the session store, every request stalled.

Everything built on the client fails with it:
`Microsoft.Extensions.Caching.StackExchangeRedis`
(`AddStackExchangeRedisCache`), the Redis output cache store, and
`PersistKeysToStackExchangeRedis`.

- **Used for:** a cache shared between instances, session state, and
  Data Protection keys.
- **What to do today:** none with this client. `IDistributedCache`, session and
  `HybridCache` work over a cache that reaches its store through asynchronous
  calls; we ran them over PostgreSQL. For Data Protection keys, see
  [Data Protection key stores](#data-protection).
- **Size of the change:** Large: a different store for the cache.
- **Can it be fixed?** Only by a change to the client library. Why the
  thread-pool configuration stalls is not yet known.

## Observability

### Exporting telemetry over OTLP {#opentelemetry}

An application that adds OpenTelemetry tracing with the OTLP exporter, from
`OpenTelemetry.Exporter.OpenTelemetryProtocol`, refuses to start: `initialization retains a timer`. The exporter collects telemetry and
sends it in batches from the background.

- **Used for:** sending traces, metrics and logs to a collector.
- **What to do today:** none that we have run.
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
  token against signing keys fetched from `MetadataAddress`.
- **`Microsoft.AspNetCore.Authentication.OpenIdConnect` 10.0.12.** The sign-in
  redirect, which fetches the provider's metadata. The callback was not
  exercised.
- **`Microsoft.EntityFrameworkCore` 10.0.12 with
  `Npgsql.EntityFrameworkCore.PostgreSQL` 10.0.3.** Asynchronous queries,
  saves, transactions and migrations, the retrying execution strategy, and
  context pooling.
- **`Microsoft.Extensions.Diagnostics.HealthChecks.EntityFrameworkCore`
  10.0.12.** `AddDbContextCheck`.
- **`Dapper` 2.1.89** with Npgsql. `QueryAsync`, `QueryFirstAsync` over a
  large result, and `ExecuteAsync`.
- **`Microsoft.Extensions.Caching.Hybrid` 10.10.0.** `GetOrCreateAsync`, with
  and without a distributed second level.
- **`Microsoft.Extensions.Http.Resilience` 10.10.0.** A request through
  `AddStandardResilienceHandler`.
- **`Serilog.AspNetCore` 10.0.0.** `UseSerilog` with the console sink.
- **`FluentValidation` 12.2.0.** Synchronous and asynchronous rules through
  `ValidateAsync`.
- **`MediatR` 12.5.0.** `Send` to a handler that queries a database.
- **`AutoMapper` 14.0.0.** Mapping an entity to a record.
- **`Swashbuckle.AspNetCore` 10.3.0.** Generating the OpenAPI document. The
  Swagger UI was not exercised.

With Npgsql, turn the driver's connection pooling off:
[Pool database connections outside the application](dotnet-runtime-differences.md#pool-database-connections-outside-the-application).

## Packages not checked yet {#not-checked}

We have not run these, so this page makes no claim about them. Where the
general rules suggest what to expect, the entry says so.

- **Azure and AWS SDK clients**, such as `Azure.Storage.Blobs` and
  `AWSSDK.S3`. Their synchronous methods fall under
  [synchronous calls](#synchronous-io). Their asynchronous methods are built
  on `HttpClient`, whose asynchronous methods work, but the clients themselves
  have not been run.
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
