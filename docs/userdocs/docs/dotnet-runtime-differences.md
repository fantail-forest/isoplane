---
title: How .NET differs on Isoplane
---

An Isoplane application is ordinary .NET code, but it runs inside a sandbox
instead of as a process on a server. Some things .NET normally lets you do are
missing there, and others behave differently. This page lists all of them. Where
another page covers one in more depth, the entry links to it.

The limits Isoplane places on memory and running time are a separate subject:
see [execution limits](execution-limits.md).

## Why the sandbox works this way

Isoplane isolates every instance of every application in a sandbox of its own.
Two choices make that isolation both strong and cheap, and the rules on this
page are what they cost.

### An application never blocks {#never-blocks}

Isoplane runs many applications on a small number of worker threads. Whenever
an application is idle, because it is waiting on an asynchronous operation or
has no request to process, Isoplane sets that instance aside and gives the
worker to another one. An instance therefore holds no thread while it waits,
which is what lets Isoplane give each one a sandbox of its own without
dedicating a thread, a process or a machine to it.

A call that blocks cannot be set aside. It would hold a worker, which other
applications share, for as long as it waited. That is why waits are
asynchronous, why the application has no threads of its own, and why nothing
runs between requests.

### Nothing runs outside the sandbox {#nothing-outside-the-sandbox}

Isoplane mediates every interaction an application has with the outside world,
including the network, files, the clock and waiting. That is what keeps one
application apart from the others on the same hardware. It is also why an
application can do only what Isoplane provides, and why a native library built
for the machine's processor is not loaded: it would run outside the sandbox.

## One thread

Each instance has exactly one thread, and `Environment.ProcessorCount` is 1.

- **Starting a thread fails.** `Thread.Start` throws
  `PlatformNotSupportedException`. The build warns about it as
  [`ISOPLANE1002`](reference/error-codes.md#isoplane1002).
- **The thread pool is that one thread.** `Task.Run` and
  `ThreadPool.QueueUserWorkItem` queue work that runs once the code that is
  running now awaits or returns. `Task.Run` does not make work parallel and does
  not move it out of the way of the request.
- **`Parallel.For` and `Parallel.ForEach` run their iterations one after
  another.**
- **Asynchronous I/O is still concurrent.** Start several operations and
  `await Task.WhenAll(...)`: they are all in flight while the instance waits.
- **Computation holds the thread.** While a method computes without awaiting,
  nothing else in the instance runs, including the other asynchronous work of the
  same request. Cancellation reaches the application only while it is waiting, so
  a long computation does not see its request being cancelled. It also uses up
  the request's [running time](execution-limits.md#running-time).
- **A lock is always free.** With one thread there is never another holder, so
  `lock`, `Monitor.Enter` and `Monitor.TryEnter`, `System.Threading.Lock`,
  `SpinLock`, `Mutex`, `ReaderWriterLockSlim` and `ReaderWriterLock` take the
  lock at once, in every mode and with or without a timeout. Entering a lock
  the code already holds behaves as it does elsewhere: a recursive lock lets it
  in, and one that refuses recursion throws `LockRecursionException`. A
  `SpinLock` that does not track its owner would wait for itself forever
  elsewhere; here it throws `PlatformNotSupportedException`.
- **A wait that can be satisfied at once succeeds.** Taking a free
  `SemaphoreSlim` or `Semaphore`, waiting on a set event or a finished task, and
  taking from a non-empty `BlockingCollection` all return straight away, as they
  do anywhere else. A wait with a timeout of zero that cannot be satisfied
  returns `false`.
- **A wait that would have to block throws.** Nothing else in the instance
  can run while the code waits, so nothing could release the semaphore, set
  the event or finish the task. Such a wait throws
  `PlatformNotSupportedException` straight away, whatever timeout it was
  given. [Waiting is asynchronous](#waiting-is-asynchronous) lists each call
  and what to use instead.
- **Thread-safe types work as usual.** `Lazy<T>` in its default thread-safe
  mode, `ConcurrentDictionary`, `ConcurrentQueue`, `ConcurrentStack` and
  `ConcurrentBag` need no change.

## Waiting is asynchronous

A synchronous wait that cannot be satisfied at once throws
`PlatformNotSupportedException` instead of waiting, even when it was given a
timeout. A wait with a timeout of zero returns `false` instead, as it does
anywhere else. `Monitor.Wait`, `Thread.Sleep` and joining the current thread
always have to wait, because only another thread could pulse the monitor or
finish the thread, so they throw whenever their timeout is not zero.

| Call | Throws | Use instead |
| --- | --- | --- |
| `task.Wait()`, `task.Result` | when the task has not finished | `await task` |
| `Task.WaitAll(...)` | when a task has not finished | `await Task.WhenAll(...)` |
| `Task.WaitAny(...)` | when no task has finished | `await Task.WhenAny(...)` |
| `semaphore.Wait()` on a `SemaphoreSlim` | when no slot is free | `await semaphore.WaitAsync()` |
| `Wait()` on a `ManualResetEventSlim` or a `CountdownEvent` | when the event is not set, or the count has not reached zero | `await` a `TaskCompletionSource` |
| `WaitOne()`, `WaitHandle.WaitAny(...)` and `WaitHandle.WaitAll(...)` on events or a `Semaphore` | when the handles are not signalled | `await` a `TaskCompletionSource`, or `await semaphore.WaitAsync()` on a `SemaphoreSlim` |
| `Take()` and `Add()` on a `BlockingCollection` | when it is empty, or full | `await` `ReadAsync` and `WriteAsync` on a `Channel` |
| `barrier.SignalAndWait()` | unless this is the last participant to arrive | — |
| `SpinWait.SpinUntil(...)` | when the condition is false | `await` the work the condition waits for |
| `Monitor.Wait(...)` | always, except with a timeout of zero | `await` a `TaskCompletionSource` |
| `Thread.Sleep(...)` | always, except `Thread.Sleep(0)` | `await Task.Delay(...)` |
| `Thread.CurrentThread.Join(...)` | always, except with a timeout of zero | — |

`task.Wait()` with no timeout, on a task that `Task.Run` queued and that has not
started yet, runs the task there and then, as .NET can do anywhere, and returns
once it has finished.

`app.Run()` is refused earlier, when the application builds: use
`await app.RunAsync()`.

The network behaves the same way. A synchronous socket call or name lookup that
would have to wait fails rather than waiting, so use the asynchronous methods.
A library's synchronous API usually blocks underneath: on an EF Core context,
call `SaveChangesAsync` and `ToListAsync`, not `SaveChanges` and `ToList`.

Some synchronous calls are easy to miss, because they do not look like I/O:

| Call | Why it waits | Use instead |
| --- | --- | --- |
| ASP.NET Core's `Request.Form`, before the form has been read | It reads the request body | `await Request.ReadFormAsync()` |
| `Read` on `Request.Body` and `Write` on `Response.Body` | They read and write the connection | `ReadAsync` and `WriteAsync` |
| Razor's `Html.Partial` and `Html.RenderPartial` | They wait for the partial view, which fails if the partial awaits anything | `<partial name="..." />`, or `await Html.PartialAsync(...)` |
| Entity Framework Core's `Add`, on an entity whose keys come from `UseHiLo` | It fetches a block of keys from the database, on the first call and whenever a block runs out, so it fails only now and then | `await AddAsync(...)` |
| `using` on a data reader that still has unread rows | Disposing it reads the remaining rows | `await using` |

Some libraries block inside a call that looks asynchronous, or behind an
interface with no asynchronous form:
[Library compatibility](library-compatibility.md) lists the ones we know of.

The build warns about several of these calls as
[`ISOPLANE1003`](reference/error-codes.md#isoplane1003), and the editor offers a quick fix
for most of them. It does not catch every case: `.Result` is not flagged, and
NuGet packages are not inspected unless you set
[`IsoplaneAnalyzeDependencies`](reference/project-settings.md#isoplaneanalyzedependencies).
The `app.Run()` error is described in
[Adapt `Program.cs`](onboard-an-application.md#adapt-programcs).

## Work belongs to a request

An application runs while it starts up and while it handles a request. Between
requests it does not run at all, unless it asks for its timers to.

- **No background work.** A loop in a `BackgroundService`, a recurring timer or a
  queue consumer has nothing to run on. An application whose `BackgroundService`
  is still running once the host has started is refused, with an error that names
  the service. Bounded asynchronous work in `IHostedService.StartAsync` is
  accepted.
- **Await everything the request needs before it responds.** Once the response is
  complete the request is over. An outbound call or name lookup that is still
  pending is cancelled and never completes, and Isoplane discards the instance:
  the next request starts from a fresh copy of the template and pays for
  activation again, with nothing reported.
- **A timer outlives its request.** When a request ends with a `Timer`,
  `Task.Delay` or `CancellationTokenSource.CancelAfter` still pending, Isoplane
  keeps the instance and the timer with it. Nothing runs between requests, so a
  timer that comes due while the instance is idle runs at the start of the next
  request on that instance, before that request's own code, until it returns or
  awaits. A timer never runs if its instance is discarded first, and an idle
  instance can be discarded at any time. So a task started and not awaited,
  such as `_ = SendEmailAsync()`, may finish during a later request, or never.
- **Timers can run between requests, at the cost of a request each time.**
  Call [`Initialization.RunTimersBetweenRequests`](reference/sdk/Isoplane.Sdk/Isoplane/Initialization/RunTimersBetweenRequests.md)
  during initialization, and Isoplane also wakes an idle instance to run the
  timers that have come due, no sooner than the minimum interval you give, of
  at least 100 milliseconds, after its last request or wake ended:

  ```csharp
  Initialization.RunTimersBetweenRequests(minimumInterval: TimeSpan.FromMinutes(1));
  ```

  An instance
  that has had no request for five minutes is stopped rather than woken again,
  and an instance Isoplane has set aside to save memory runs its timers only
  when its next request arrives. Use it for upkeep, such as sending telemetry
  that would otherwise wait for the next request, and not for work that has to
  happen on schedule.

  To show what that costs, in local development and tests Isoplane logs a
  notice for each periodic timer once the application has opted in: how often
  the timer fires, and how many wakes the instance's timers can cause, as "up
  to N per hour (M per month) while the app is receiving at least one request
  per 5 minutes". A wake runs every timer that is due, so the figure is a
  ceiling. Deployments do not log these notices.
- **`finally` blocks and disposal are part of the request.** They run as the
  awaited code unwinds, as they do anywhere else, and that includes middleware
  code after `await next()` and the disposal of scoped services. Two cases
  differ. Work in them that has to wait for I/O after the response is complete
  may not finish. And when Isoplane stops an instance because a request went
  over its [limits](execution-limits.md), they do not run at all.
- **Finalizers may never run.** There is no finalizer thread: a finalizer runs on
  the application's thread during a garbage collection. That can be in a later
  request, whose running time it then uses, and it does not happen if the
  instance is discarded first. Release what matters with `Dispose` or
  `DisposeAsync`.
- **Nothing else is guaranteed to run after the response.** Code that is ready to
  run at that moment may still run while the request is within its limits. Do
  not rely on it to save data or to release something.
- **Forward the request's cancellation token to the calls you await.** That token
  is `HttpContext.RequestAborted`, or
  [`RequestHead.Aborted`](reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
  without ASP.NET. It is cancelled when the client goes away or the request uses
  up its running time, so work nobody is waiting for stops.

## Instances come and go

Isoplane decides how many instances of an application run, and may discard one
after any request. Successive requests can reach different instances.

- **Memory is a cache, not storage.** Static fields, `IMemoryCache` and in-memory
  session state belong to one instance and can disappear. Keep anything that
  must last in a database or another external service.
- **Open connections may not survive between requests.** A stale connection is
  possible even after a request that completed normally.
- **Initialization state is captured and copied.** `Program.cs` runs up to `RunAsync` while
  Isoplane prepares the application, and every instance starts from a copy of
  that memory. A connection opened during initialization cannot be carried
  into an instance, so close it before initialization finishes.
- **A value created during initialization is the same in every instance.** If
  it has to be unique to each instance, as an identifier does, replace it when
  the instance activates, by subscribing to `Isoplane.Activation.Activated`.
- **Random numbers are reseeded for you, with exceptions.** Each instance
  reseeds `Random.Shared`, every `Random` created without a seed and SQLite's
  `random()`, and cryptographic randomness is always drawn fresh. A `Random`
  you gave a seed, another generator that is not a `System.Random`, and a value
  already drawn during initialization are copied as they are.
- **ASP.NET Core Data Protection keys are shared within a template.** Instances
  use the keys captured in that template. Fresh initialization may create a
  different key; reusing a template retains its key. [Keep the keys across
  builds](#keep-data-protection-keys-across-builds) while earlier protected data
  must remain readable.

[The application lifecycle](application-lifecycle.md) explains each of these in
more depth.

### Keep Data Protection keys across builds

By default a key created during initialization is captured in the template.
Fresh initialization may create a different key; reusing a prepared template
retains its captured key. Keep earlier keys while data protected with them
must remain readable, using one of these stores.

**From a secret in the application's settings.** Add one line where you
configure services:

```csharp
builder.Services.AddDataProtection().UseConfiguredKeys();
```

It reads `DataProtection:Keys:0`, which must be at least 32 random bytes in
Base64. For local development, keep it in User Secrets:

```bash
dotnet user-secrets set "DataProtection:Keys:0" "$(openssl rand -base64 48)"
```

The application refuses to start without a usable secret. To replace a key,
put the new secret at `DataProtection:Keys:0` and move the old one to
`DataProtection:Keys:1`: the first protects new data and every one listed still
unprotects, so remove the old one once nothing it protected is in use.

**From the application's database.** If the application uses Entity Framework
Core, add the
[`Isoplane.AspNetCore.DataProtection.EntityFrameworkCore`](reference/sdk/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore.md)
package at the version of your Isoplane SDK:

```bash
dotnet add package Isoplane.AspNetCore.DataProtection.EntityFrameworkCore --version VERSION
```

It brings in `Microsoft.AspNetCore.DataProtection.EntityFrameworkCore`. Add that
package's `DataProtectionKeys` table to your context as it describes, and write:

```csharp
using Isoplane.AspNetCore.DataProtection.EntityFrameworkCore;

builder.Services.AddDataProtection().UseKeysFromDbContext<AppDbContext>();
```

[`UseKeysFromDbContext`](reference/sdk/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/Isoplane.AspNetCore.DataProtection.EntityFrameworkCore/DataProtectionDbContextKeys/UseKeysFromDbContext.md)
is in a package of its own so that an application that does not use Entity
Framework Core never restores it.

Initialization loads the stored keys and can create and store a key if none is usable. The keys are
read once, when initialization finishes, so the table must exist by then: a
migration applied in an `Initialization.Initializing` callback has run. If such
a callback itself protects or unprotects data, for instance by warming up a
page that renders a form, load the keys first:

```csharp
Initialization.Initializing += async () =>
{
    await database.MigrateAsync();
    await app.LoadDataProtectionKeysAsync();
    await app.WarmupAsync("/");
};
```

The [`isoplane-webapp`](reference/project-templates.md#isoplane-webapp)
template is set up this way.

A prepared template retains the key ring loaded during initialization, even
after keys expire. When initialization loads the store, it creates and persists
a new key if no unrevoked active key remains valid beyond the renewal window.
The new key uses the configured key lifetime. Reusing a prepared template
neither reloads nor renews its key ring. For a store other than Entity Framework Core,
`UseKeysFrom` takes the two asynchronous functions that read and add keys.

ASP.NET Core's own key stores for a database or another service, such as
`PersistKeysToDbContext`, do not work here. Data Protection reads and writes
its keys synchronously, and an application in the sandbox cannot block on the
network, so reading the keys fails with `PlatformNotSupportedException`.

### Pool database connections outside the application

A connection pool inside the application belongs to one instance. Its size
limit applies to that instance alone, so it does not cap the connections your
application as a whole opens: ten instances with a pool of twenty can hold two
hundred. The pool is also lost whenever its instance is discarded, and a
connection it kept open may be stale by the next request.

Put the pool in front of the database instead. Use your database provider's
pooler, such as PgBouncer or a provider's pooled connection endpoint, and point
the application's connection string at it. The pooler caps the total and keeps
connecting cheap, and the application can open a connection for each request
and dispose it when the request is done.
[Connect to PostgreSQL](connect-postgresql.md) shows a service that does this,
with the driver's own pooling off.

## Network

Inbound, an application receives HTTP requests and nothing else.

- It cannot listen on a port or accept connections of its own.
- Requests arrive as HTTP/1.1, without connection upgrades or response trailers.
  WebSockets are unavailable, and so are SignalR's WebSocket transport, Blazor
  Server interactivity and gRPC over HTTP/2. gRPC-Web and statically rendered
  Blazor pages work. See
  [Check application assumptions](onboard-an-application.md#check-application-assumptions).

Outbound, an application can open TCP connections through the asynchronous
APIs, which covers `HttpClient`, TLS and database drivers.

- There is no UDP.
- **Socket options apply within limits Isoplane sets.** `NoDelay`, keep-alive
  and the receive and send buffer sizes work as elsewhere, but a value outside
  the supported range is clamped to it, and reading an option back returns the
  value in force:
  - **Buffer sizes:** 4,096 to 212,992 bytes. An unset buffer size reads back
    as 65,536.
  - **Keep-alive idle time:** 10 to 32,767 seconds.
  - **Keep-alive interval:** 1 to 32,767 seconds.
  - **Keep-alive probe count:** 1 to 127.

  Setting or reading the hop limit (`Socket.Ttl`) throws `SocketException`
  with `OperationNotSupported`.
- A client from `IHttpClientFactory` opens a connection for each call and
  closes it once the response has been read, so it keeps no connection between
  calls. Elsewhere a client keeps idle connections for its next calls.
  Isoplane switches that off because a connection still open when
  [initialization](application-lifecycle.md#initialization) ends stops the
  application starting, and connections kept between requests count toward the
  instance's [outbound connections](execution-limits.md#outbound-connections).
  The cost is a new connection, and over HTTPS a new TLS handshake, for each
  call, including calls a request makes one after another to the same host.
- Idle connections come back if you ask for them: any
  `PooledConnectionLifetime` but zero on the client's `SocketsHttpHandler`, or
  a primary handler of your own from `ConfigurePrimaryHttpMessageHandler`. Give
  a handler of your own `PooledConnectionLifetime = TimeSpan.Zero`. A
  `PooledConnectionIdleTimeout` of zero is no substitute: it leaves each
  connection open, and calls fail once the application has used up its
  outbound connections.
- A client you construct yourself with `new HttpClient()` and keep for later
  requests keeps its idle connections open between requests, and they count
  toward the instance's [outbound connections](execution-limits.md#outbound-connections).
  The timer that removes them runs only during a request. Take the client from
  `IHttpClientFactory` instead, or construct it with
  `new HttpClient(new SocketsHttpHandler { PooledConnectionLifetime = TimeSpan.Zero })`.
  A client created and disposed within one request reuses its connection for
  that request's calls and closes it when it is disposed.
- TLS certificates are validated as usual. Switching validation off is not a fix
  for a connection that fails.

## The machine underneath

- **The file system is read-only.** It holds the files of your application
  package, such as `appsettings.json` and `wwwroot`, and nothing else. Creating
  or writing a file fails, so there is nowhere to put temporary files. Keep uploads
  and generated files in memory while you handle the request, or in external
  storage.
- **A file's modification time is the one it had when the application was
  built**, to the second. ASP.NET Core sends it as the `Last-Modified` header
  of a static file, as it does anywhere else. `UseStaticFiles` sends no
  `Cache-Control` header, so a browser may keep such a file for a while without
  asking whether it has changed, here as on any host: `MapStaticAssets`, or a
  link with `asp-append-version`, avoids that.
- **Environment variables are empty.** Settings reach the application through
  configuration; see [local development settings](local-settings.md).
- **There are no other processes.** An application cannot start one.
- **Native libraries come from the runtime.** Isoplane's runtime includes the
  native parts of .NET, such as compression, globalization and OpenSSL, and
  [SQLite](library-compatibility.md#sqlite) for in-memory databases, all
  compiled to WebAssembly so that they run inside the sandbox. An
  application's package carries managed code only, so it cannot add a native
  library of its own, and the Linux, macOS and Windows binaries a NuGet package
  ships are not loaded. A P/Invoke into a library the runtime does not include
  fails when it is called, not when the application builds or starts. A NuGet
  package that ships a native binary therefore still works as long as the code
  you use stays in managed code, and fails only on a path that reaches the
  native call. The build reports such declarations as
  [`ISOPLANE1001`](reference/error-codes.md#isoplane1001) when you set
  [`IsoplaneWarnOnNativeImports`](reference/project-settings.md#isoplanewarnonnativeimports).
- **The process is 32-bit.** `Environment.Is64BitProcess` is `false`.
- **Local time is UTC**, and the default culture is the invariant culture. Named
  cultures and time zones are available when you ask for them.

## Time

The clock an application sees is deliberately coarse. A precise timer is what
timing attacks between applications on shared hardware depend on, so Isoplane
does not provide one.

- **Time stands still while code runs.** The clock is set when a request starts
  and each time the application resumes after waiting. Between those points
  every reading returns the same value, from `DateTime.UtcNow`, `Stopwatch` and
  `Environment.TickCount64` alike.
- **Resolution is one millisecond.** `Stopwatch.IsHighResolution` still reports
  `true`, and `Stopwatch.Frequency` still reports nanoseconds. Timers have the
  same resolution.
- **Timers and cancellation stand still too.** A timer that comes due, or a
  cancellation token that is cancelled, while code runs is seen only after the
  application next waits. Checking a timer or `IsCancellationRequested` in a
  loop that does not await never sees it change.

What follows from that:

- `Stopwatch` cannot time a piece of computation, and a benchmark run inside
  the application measures nothing. Across an `await` it measures the wait, to
  the millisecond.
- Timestamps the application takes without waiting in between are identical, so
  log lines it stamps itself can all show the same time.
- A loop that computes and checks the time for its own deadline never sees the
  deadline arrive.
- A timeout only fires while the application is waiting. `Task.Delay`,
  `CancellationTokenSource.CancelAfter` and `Timer` callbacks are delivered when
  it next waits, not in the middle of a computation, or at the start of the
  next request if they came due between requests, unless the application
  [runs timers between requests](#work-belongs-to-a-request).

## How code runs

Your code runs as IL on an interpreter. There is no JIT compiler, and nothing is
trimmed or compiled ahead of time, so reflection, runtime type loading,
`Reflection.Emit` and compiled expressions all work.

The SDK version in the project file, the number in
`Sdk="Isoplane.AspNetCore/..."`, also selects the runtime the application runs
on. Debug and Release run on that same runtime and differ only in how the C#
compiler optimizes your code.
