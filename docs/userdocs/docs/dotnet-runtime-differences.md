---
title: How .NET differs on Isoplane
---

An Isoplane application is ordinary .NET code, but it runs inside a sandbox
instead of as a process on a server. Each instance of each application gets a
sandbox of its own, and everything it does beyond its own memory, from the
network and files to the clock, goes through Isoplane. To make that cheap,
Isoplane gives an instance a thread only while its code is running: an instance
that is waiting, or has no request to handle, is set aside and holds no thread
at all.

Most of the differences on this page follow from those two choices. Some things
.NET normally lets you do are missing, and others behave differently. Each
section says why, what that gives you, and what to change in your application,
where there is anything to change. Where another page covers a difference in
more depth, the section links to it.

The limits Isoplane places on memory and running time are a separate subject:
see [execution limits](execution-limits.md).

## A single-threaded instance {#single-threaded}

Each instance runs your application on exactly one thread, and handles one
request at a time; `Environment.ProcessorCount` is 1. To serve more requests at
once, Isoplane runs more instances of the application rather than more threads
in one. An instance needs a thread only while its code runs, and one is all it
is given. In return, an
instance costs no thread while it waits, and code in an instance never runs on
two threads at once.

Starting a thread throws `PlatformNotSupportedException`, and the build warns
about it as [`ISOPLANE1002`](reference/error-codes.md#isoplane1002). Rewrite
such work as asynchronous code that the request awaits.

The thread pool is the same thread. `Task.Run` and
`ThreadPool.QueueUserWorkItem` queue work that runs once the code running now
awaits or returns, so `Task.Run` neither makes work parallel nor moves it out
of the request's way. `Parallel.For` and `Parallel.ForEach` run their
iterations one after another. Code that uses any of these still works,
without the speed-up it was written for.

Asynchronous I/O is still concurrent, and it is how to make a request faster
here. Start several operations and `await Task.WhenAll(...)`, and all of them
are in flight while the instance waits.

Computation, by contrast, keeps the thread. While a method computes without
awaiting, nothing else in the instance runs, including other asynchronous work
of the same request, and the request's cancellation token does not change
until the method next awaits. A long computation also uses up the request's
[running time](execution-limits.md#running-time), which explains how to split
one so that cancellation can reach it.

Thread-safe types need no change. `Lazy<T>` in its default thread-safe mode,
`ConcurrentDictionary`, `ConcurrentQueue`, `ConcurrentStack` and
`ConcurrentBag` work as usual.

## How locks work {#locks}

Locks work as they do elsewhere, with one thread to own them. A lock that is
free is taken at once, by `lock`, `Monitor.Enter` and `Monitor.TryEnter`,
`System.Threading.Lock`, `SpinLock`, `Mutex`, `ReaderWriterLockSlim` and
`ReaderWriterLock`, in every mode and with or without a timeout. A lock that
is held is held by this same thread, so taking it again re-enters it: a lock
that allows recursion lets the code in, and one that does not throws
`LockRecursionException`, just as a thread re-entering its own lock would
anywhere else. A `SpinLock` that does not track its owner would wait for
itself forever elsewhere; here it throws `PlatformNotSupportedException`.

A lock therefore never keeps out other work in the same instance, and code
that takes locks needs no change to run here. To keep other asynchronous work
out of a section that awaits, use `SemaphoreSlim` with `await WaitAsync()`, as
you would on any platform, since no lock can be held across an `await`.

## Waits, delays and timers {#waits}

Waiting is asynchronous. A waiting instance is set aside and holds no thread,
which is how many applications share a few worker threads, and its waiting
does not count toward the request's
[running time](execution-limits.md#running-time). A wait that blocked would
keep the thread, which other applications need.

A wait that can be satisfied at once succeeds, as it does anywhere else:
taking a free `SemaphoreSlim` or `Semaphore`, waiting on a set event or a
finished task, and taking from a non-empty `BlockingCollection` all return
straight away, and a wait with a timeout of zero that cannot be satisfied
returns `false`.

A synchronous wait that would have to block throws
`PlatformNotSupportedException` straight away, whatever timeout it was given.
Nothing else in the instance can run while it waits, so nothing could release
the semaphore, set the event or finish the task, and the wait could only hold
the worker until it timed out; failing at once points you at the line to
change. `Monitor.Wait`, `Thread.Sleep` and joining the current thread always
have to wait, because only another thread could pulse the monitor or finish
the thread, so they throw whenever their timeout is not zero. Use the
asynchronous form of each call:

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

To wait for a while, `await Task.Delay(...)`. Delays and timers are as precise
as the [clock](#time), one millisecond, and a timer that comes due while the
application is computing runs once it next awaits. A timer can also outlive
the request that started it: [what can run between requests?](#between-requests)
describes what happens to it then.

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

## The clock {#time}

The clock an application sees is deliberately coarse, and it stands still while
code runs. This is a defence against Spectre and other side-channel attacks,
which work by timing tiny differences in how long the processor takes, and so
need a precise clock. Hardware is shared between applications, and denying
every application such a clock protects yours from its neighbours, and theirs
from yours.

The clock is set when a request starts and each time the application resumes
after waiting. Between those points every reading returns the same value, from
`DateTime.UtcNow`, `Stopwatch` and `Environment.TickCount64` alike. Its
resolution is one millisecond, although `Stopwatch.IsHighResolution` still
reports `true` and `Stopwatch.Frequency` still reports nanoseconds. Timers have
the same resolution.

Timers and cancellation stand still in the same way. A timer that comes due,
or a cancellation token that is cancelled, while code runs is seen only after
the application next waits. Checking a timer or `IsCancellationRequested` in a
loop that does not await never sees it change.

What follows from that:

- `Stopwatch` cannot time a piece of computation, and a benchmark run inside
  the application measures nothing. Run benchmarks on ordinary .NET outside
  Isoplane.
- Across an `await` that has to wait, `Stopwatch` measures from when the
  application last started running or resumed before you started it, to when
  it resumed after the wait. That includes the wait and any computation since
  that earlier point, so it is an upper bound on the wait, to the millisecond.
  An `await` on something already complete does not advance it.
- Timestamps the application takes without waiting in between are identical,
  so log lines it stamps itself can all show the same time.
- A loop that computes and checks the time, a timer or a cancellation token for
  its own deadline never sees the deadline arrive. Split the work into parts
  and await something that waits between them, as
  [running time](execution-limits.md#running-time) describes.
- A timeout fires only while the application is waiting. `Task.Delay`,
  `CancellationTokenSource.CancelAfter` and `Timer` callbacks are delivered
  when it next waits, not in the middle of a computation, or at the start of
  the next request if they came due between requests, unless the application
  [runs timers between requests](#run-timers-between-requests).

## When your code runs

An application's code runs in three situations: while it starts up, while it
handles a request, and, if the application asks for this, when Isoplane wakes
it to run its timers. The rest of the time an instance is idle. It holds no thread and
uses no processor time, and Isoplane can set it aside or discard it whenever it
needs the room. That is what lets an application that is waiting, or that
nobody is using, cost so little.

### How Isoplane accounts for work

The request is the unit of work. Each request has an allowance of [running time](execution-limits.md#running-time):
the time its code spends running, not the time it spends waiting for a database
or another service. A wake to run timers between requests counts as one
request too. Code that runs at the start of a request because a timer came due
uses that request's running time.

Forward the request's cancellation token to the calls you await. That token is
`HttpContext.RequestAborted`, or
[`RequestHead.Aborted`](reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
without ASP.NET. It is cancelled when the client goes away or the request uses
up its running time, so passing it on stops work that nobody is waiting for,
and saves the running time that work would use.

### What has to finish before the response? {#before-the-response}

Everything the response depends on. Once the response is complete the request
is over, and an outbound call or name lookup that is still pending is cancelled
and never completes. The instance is discarded along with it, because the code
waiting on that call could never resume: the next request starts from a fresh
copy of the template and pays for activation again, with nothing reported.

Other code that is ready to run when the response completes may still run while
the request is within its limits, but nothing guarantees it. Do not leave saving
data or releasing a resource to it. Work that has to happen after the response
belongs in another service, such as a queue that something else reads.

### What can run between requests? {#between-requests}

Timers can. A timer outlives the request that started it: when a request ends
with a `Timer`, `Task.Delay` or `CancellationTokenSource.CancelAfter` still
pending, Isoplane keeps the instance and the timer with it. Libraries that keep
timers of their own, such as rate limiters, connection pools and telemetry
exporters, therefore work without change.

By default a timer that comes due while the instance is idle waits for the next
request on that instance, and runs at its start, before the request's own code,
until it returns or awaits. A timer never runs if its instance is discarded
first, and an idle instance can be discarded at any time. A task started and not
awaited, such as `_ = SendEmailAsync()`, may therefore finish during a later
request, or never: await the work the response depends on.

#### Running timers between requests {#run-timers-between-requests}

An application can also have its timers run while it has no request to handle,
at the cost of a request each time an instance is woken for them. Call
[`Initialization.RunTimersBetweenRequests`](reference/sdk/Isoplane.Sdk/Isoplane/Initialization/RunTimersBetweenRequests.md)
during initialization, and Isoplane wakes an idle instance to run the timers
that have come due, no sooner than the minimum interval you give after its last
request or wake ended. The interval must be at least 100 milliseconds:

```csharp
Initialization.RunTimersBetweenRequests(minimumInterval: TimeSpan.FromMinutes(1));
```

An instance
that has had no request for five minutes is stopped rather than woken again,
and an instance Isoplane has set aside to save memory runs its timers only when
its next request arrives. Use it for upkeep, such as sending telemetry that
would otherwise wait for the next request, and not for work that has to happen
on schedule.

### What has nowhere to run? {#nowhere-to-run}

Work that is meant to run continuously, or on a schedule it must keep, such as
a loop in a `BackgroundService`, a queue consumer, or a job scheduler. Timer
wakes are best effort and stop once an instance has gone five minutes without
a request, so they cannot carry such work, and nothing else runs between
requests. An application whose `BackgroundService` is still running once the
host has started is refused, with an error that names the service. Bounded
asynchronous work in `IHostedService.StartAsync` is accepted.

Start such work from a request instead: for example, from an endpoint that a
scheduler or a queue service calls.

### Cleanup

`finally` blocks and disposal are part of the request. They run as the awaited
code unwinds, as they do anywhere else, and that includes middleware code after
`await next()` and the disposal of scoped services. Two cases differ. Work in
them that has to wait for I/O after the response is complete may not finish.
And when Isoplane stops an instance because a request went over its
[limits](execution-limits.md), they do not run at all. Keep cleanup quick and
independent of I/O after the response.

Finalizers may never run. With one thread, there is no finalizer thread: a
finalizer runs on the application's thread during a garbage collection. That
can be in a later request, whose running time it then uses, and it does not
happen if the instance is discarded first. Release what matters with `Dispose`
or `DisposeAsync`.

## Instances come and go

Isoplane decides how many instances of an application run, starting more as
traffic grows and discarding them when they are idle or the memory is needed.
Every instance starts from a copy of the application as it stood when
initialization finished, so a new instance starts without running your startup
code again. Successive requests can reach different
instances, and any instance can be discarded after any request.

Memory is a cache, not storage. Static fields, `IMemoryCache` and in-memory
session state belong to one instance and can disappear. Keep anything that
must last in a database or another external service.

A connection kept from an earlier request may be stale, even after a request
that completed normally. Be ready to open a new one when it fails, as a
connection pool would anywhere.

Initialization state is captured and copied. `Program.cs` runs up to `RunAsync`
while Isoplane prepares the application, and every instance starts from a copy
of that memory. That is why expensive startup work, such as building caches or
warming up routes, belongs in initialization: the captured result is reused by
instances. A connection opened during initialization cannot be carried into an
instance, so close it before initialization finishes.

A value created during initialization is the same in every instance. If it has
to be unique to each instance, as an identifier does, replace it when the
instance activates, by subscribing to `Isoplane.Activation.Activated`.

Random numbers are reseeded for you, with exceptions. Each instance reseeds
`Random.Shared`, every `Random` created without a seed and SQLite's
`random()`, and cryptographic randomness is always drawn fresh. A `Random` you
gave a seed, another generator that is not a `System.Random`, and a value
already drawn during initialization are copied as they are. Create those in an
`Activation.Activated` callback when each instance needs its own.

ASP.NET Core Data Protection keys are shared by instances of the same template,
so antiforgery tokens and authentication cookies work across those instances.
Fresh initialization may create a different key; reusing a template retains its
key. To read older protected data, [keep its keys](#keep-data-protection-keys-across-builds).

[The application lifecycle](application-lifecycle.md) explains each of these in
more depth.

### Keep Data Protection keys across builds

By default a key created during initialization is captured in the template.
Fresh initialization may create a different key; reusing a prepared template
retains its captured key. Keep earlier keys while data protected with them
must remain readable, using one of these stores.

#### From a secret in the application's settings

Add one line where you configure services:

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

#### From the application's database

If the application uses Entity Framework Core, add the
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

### Incoming requests {#incoming-requests}

An application receives HTTP requests and nothing else. Isoplane
accepts the connections and delivers each request to an instance, which is what
lets it send a request to whichever instance is free and start more as traffic
grows. An application therefore cannot listen on a port or accept connections
of its own.

Requests arrive as HTTP/1.1, without connection upgrades or response trailers.
WebSockets are unavailable, and so are SignalR's WebSocket transport, Blazor
Server interactivity and gRPC over HTTP/2. gRPC-Web and statically rendered
Blazor pages work. See
[Check application assumptions](onboard-an-application.md#check-application-assumptions).

### Outgoing connections {#outgoing-connections}

An application can open TCP connections through the asynchronous
APIs, which covers `HttpClient`, TLS and database drivers. Isoplane makes each
connection on the application's behalf, which is how a connection stays open
while its instance is set aside and how the limits that protect other
applications are applied. There is no UDP: where a library offers a choice of
transport, such as a telemetry exporter, choose one over TCP, such as HTTP.

TLS certificates are validated as usual. Switching validation off is not a fix
for a connection that fails.

Socket options apply within ranges Isoplane sets for the connections it makes
on the application's behalf. `NoDelay`, keep-alive and the receive and send
buffer sizes work as elsewhere, but a value outside its range is clamped to
it, and reading an option back returns the value in force:

| Option | Range |
| --- | --- |
| Receive and send buffer sizes | 4,096 to 212,992 bytes; an unset size reads back as 65,536 |
| Keep-alive idle time | 10 to 32,767 seconds |
| Keep-alive interval | 1 to 32,767 seconds |
| Keep-alive probe count | 1 to 127 |

Setting or reading the hop limit (`Socket.Ttl`) throws `SocketException` with
`OperationNotSupported`.

### HTTP clients and connection reuse {#http-clients}

A client from `IHttpClientFactory` opens a connection for each call and closes
it once the response has been read, so it keeps no connection between calls.
Elsewhere a client keeps idle connections for its next calls. Isoplane switches
that off because a connection still open when
[initialization](application-lifecycle.md#initialization) ends stops the
application starting, and connections kept between requests count toward the
instance's [outbound connections](execution-limits.md#outbound-connections).
The cost is a new connection, and over HTTPS a new TLS handshake, for each
call, including calls a request makes one after another to the same host.

Idle connections come back if you ask for them: any `PooledConnectionLifetime`
but zero on the client's `SocketsHttpHandler`, or a primary handler of your own
from `ConfigurePrimaryHttpMessageHandler`. Give a handler of your own
`PooledConnectionLifetime = TimeSpan.Zero`. A `PooledConnectionIdleTimeout` of
zero is no substitute: it leaves each connection open, and calls fail once the
application has used up its outbound connections.

A client you construct yourself with `new HttpClient()` and keep for later
requests keeps its idle connections open between requests, and they count
toward the instance's
[outbound connections](execution-limits.md#outbound-connections). The timer
that removes them runs only during a request. Take the client from
`IHttpClientFactory` instead, or construct it with
`new HttpClient(new SocketsHttpHandler { PooledConnectionLifetime = TimeSpan.Zero })`.
A client created and disposed within one request reuses its connection for that
request's calls and closes it when it is disposed.

## File system {#file-system}

The file system is read-only. It holds the files of your application package,
such as `appsettings.json` and `wwwroot`, and nothing else. Every instance
starts from the same build, and anything one wrote would vanish with it and be
missing from the others, so a read-only file system keeps instances identical
and keeps you from relying on a file that will not be there. Creating or
writing a file fails, so there is nowhere to put temporary files. Keep uploads
and generated files in memory while you handle the request, or in external
storage.

A file's modification time is the one it had when the application was built,
to the second. ASP.NET Core sends it as the `Last-Modified` header of a static
file, as it does anywhere else. `UseStaticFiles` sends no `Cache-Control`
header, so a browser may keep such a file for a while without asking whether it
has changed, here as on any host: `MapStaticAssets`, or a link with
`asp-append-version`, avoids that.

## Environment and processes {#environment-and-processes}

Environment variables are empty, because an instance has no process
environment of its own. Settings reach the application through .NET
configuration instead, so read them from `IConfiguration`; see
[local development settings](local-settings.md).

There are no other processes, and an application cannot start one: a process
would run outside the sandbox. Use a managed
library for the work instead, or call a service that does it.

## Native libraries {#native-libraries}

Native libraries come from the runtime. Isoplane's runtime includes the native
parts of .NET, such as compression, globalization and OpenSSL, and
[SQLite](library-compatibility.md#sqlite) for in-memory databases, all compiled
to WebAssembly so that they run inside the sandbox. An application's package
carries managed code only, so it cannot add a native library of its own, and
the Linux, macOS and Windows binaries a NuGet package ships are not loaded. A
P/Invoke into a library the runtime does not include fails when it is called,
not when the application builds or starts. A NuGet package that ships a native
binary therefore still works as long as the code you use stays in managed code,
and fails only on a path that reaches the native call. The build reports such
declarations as [`ISOPLANE1001`](reference/error-codes.md#isoplane1001) when you
set [`IsoplaneWarnOnNativeImports`](reference/project-settings.md#isoplanewarnonnativeimports).

## Culture and time zone {#culture-and-time-zone}

Local time is UTC, and the default culture is the invariant culture. An
instance has no machine of its own to take a time zone or a locale from, so
the defaults are the same wherever it runs. Named cultures and time zones are
all available when you ask for them by name: set the culture for a request
with ASP.NET Core's request localization, or pass a `CultureInfo` to the call
that formats a value, and convert times with
`TimeZoneInfo.FindSystemTimeZoneById`.

## How code runs

Your code runs as IL on an interpreter. There is no JIT compiler, and nothing is
trimmed or compiled ahead of time, so reflection, runtime type loading,
`Reflection.Emit` and compiled expressions all work, and libraries that depend
on them need no change.

The process is 32-bit, because WebAssembly addresses memory with 32-bit
pointers, and `Environment.Is64BitProcess` is `false`. An instance's memory is
far smaller than 32 bits can address, so this matters only to code that checks
the value or assumes 64-bit pointers.

The SDK version in the project file, the number in
`Sdk="Isoplane.AspNetCore/..."`, also selects the runtime the application runs
on. Debug and Release run on that same runtime and differ only in how the C#
compiler optimizes your code.
