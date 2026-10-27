---
title: How .NET differs on Isoplane
---

An Isoplane application is ordinary .NET code running in a sandbox rather than
a process. Most of what differs follows from three facts:

1. An instance is single-threaded, and occupies its thread only while code is
   running.
2. Instances are copies of your initialized application, created on demand
   and *evicted*, meaning discarded along with everything in their memory,
   when we no longer need them.
3. All I/O and system operations are transparently delegated to Isoplane.

Together they are how we scale your application out on demand and keep it
cheap to run. For a typical async ASP.NET Core application the differences
amount to a handful of changes, and where this page says nothing, .NET behaves
as usual. Memory and CPU limits are covered in
[resource limits](resource-limits.md).

## Single-threaded execution {#single-threaded}

Each instance runs on exactly one thread, and `Environment.ProcessorCount`
is 1. Several requests can be in flight on an instance at once, interleaved on
that thread, and we start more instances when one is not enough. Requests on
the same instance share static state, as on any server, and ordinary async
ASP.NET Core code runs unchanged.

### Dispatches and slices {#dispatches-and-slices}

A *dispatch* is a unit of work we give an instance: an HTTP request, or
[background work](#background-work) such as a timer callback. A *slice* is an
uninterrupted run of a dispatch's code on the thread. A slice ends when an
`await` suspends or the dispatch finishes. Slices are never preempted, and two
slices never run in parallel within a single instance.

<svg viewBox="0 0 720 170" role="img" aria-label="Three requests on one instance, each running in several slices separated by waits" style={{width: '100%', maxWidth: '720px', display: 'block', margin: '1rem 0'}} fontFamily="var(--ifm-font-family-base, sans-serif)" fontSize="13">
  <text x="90" y="44" textAnchor="end" fill="currentColor">Request 1</text>
  <rect x="100.0" y="29" width="66.7" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="166.7" y1="40" x2="300.0" y2="40" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="300.0" y="29" width="66.7" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="366.7" y1="40" x2="533.3" y2="40" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="533.3" y="29" width="50.0" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <text x="90" y="84" textAnchor="end" fill="currentColor">Request 2</text>
  <rect x="175.0" y="69" width="66.7" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="241.7" y1="80" x2="383.3" y2="80" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="383.3" y="69" width="66.7" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="450.0" y1="80" x2="591.7" y2="80" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="591.7" y="69" width="50.0" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <text x="90" y="124" textAnchor="end" fill="currentColor">Request 3</text>
  <rect x="450.0" y="109" width="66.7" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="516.7" y1="120" x2="650.0" y2="120" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="650.0" y="109" width="50.0" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="100" y1="146" x2="700.0" y2="146" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <line x1="100.0" y1="146" x2="100.0" y2="151" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="100.0" y="165" textAnchor="middle" fill="currentColor" opacity="0.8">0 ms</text>
  <line x1="183.3" y1="146" x2="183.3" y2="151" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="183.3" y="165" textAnchor="middle" fill="currentColor" opacity="0.8">50</text>
  <line x1="266.7" y1="146" x2="266.7" y2="151" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="266.7" y="165" textAnchor="middle" fill="currentColor" opacity="0.8">100</text>
  <line x1="350.0" y1="146" x2="350.0" y2="151" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="350.0" y="165" textAnchor="middle" fill="currentColor" opacity="0.8">150</text>
  <line x1="433.3" y1="146" x2="433.3" y2="151" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="433.3" y="165" textAnchor="middle" fill="currentColor" opacity="0.8">200</text>
  <line x1="516.7" y1="146" x2="516.7" y2="151" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="516.7" y="165" textAnchor="middle" fill="currentColor" opacity="0.8">250</text>
  <line x1="600.0" y1="146" x2="600.0" y2="151" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="600.0" y="165" textAnchor="middle" fill="currentColor" opacity="0.8">300</text>
  <line x1="683.3" y1="146" x2="683.3" y2="151" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="683.3" y="165" textAnchor="middle" fill="currentColor" opacity="0.8">350</text>
</svg>

Each row is a request, each bar a slice, and each gap a wait.

The gaps are the point. While a request waits on a database or a downstream
service, its instance occupies no thread, and we run slices from other
instances in that time. No thread is ever tied up waiting, so a host serves
far more applications than it has threads. This property of our architecture
is what lets us deliver such highly scalable hosting at a very low cost.

Your code runs only inside dispatches, apart from initialization, which runs
during template preparation before any instance exists. Between slices an instance occupies
no thread, and we can evict it at any time.

### Running time {#running-time}

*Running time* measures occupied worker intervals for an instance's slices and
host operations on its behalf, including time a worker is descheduled. Parked
asynchronous waits do not count. Each request has a
[running-time allowance](resource-limits.md#running-time), and background
work has a [separate one](resource-limits.md#background-allowance), topped up
over time and with whatever requests leave unused.

Every request gets a cancellation token: `HttpContext.RequestAborted`, or
[`RequestHead.Aborted`](../reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
without ASP.NET. It is triggered when the allowance is exhausted, and when the
client disconnects. A cancelled request then has a 5-second grace period to
finish.

:::important[Honour cancellation promptly]
If a cancelled request is still running when its grace period ends, its
instance is [evicted](#ephemeral-instances), and every other request on
that instance fails with it.
Pass the token to everything you await and let `OperationCanceledException`
end the request.
[Running time](resource-limits.md#running-time) has the
details.
:::

### Threads and the thread pool {#thread-pool}

An instance has one thread, so there is nothing for a second one to run on.
`new Thread(...).Start()` throws `PlatformNotSupportedException`, and the
analyzer reports it as
[`ISOPLANE1002`](../reference/error-codes.md#isoplane1002).

:::tip
Rewrite thread-based work as async code that the request awaits.
:::

`Task.Run`, `ThreadPool.QueueUserWorkItem` and `Parallel.For` still work, but
on the same thread. Queued work runs when your code next awaits, and parallel
loops run sequentially. They lose their speed-up here but don't need removing.

Async I/O is still concurrent, and `await Task.WhenAll(...)` over several
operations is how you make a request faster. CPU-bound work is not.

:::important[Keep CPU-bound work short]
While a method computes without awaiting, nothing else on that instance runs,
and cancellation can't reach it. I/O-bound requests, which is most of them,
are unaffected, and we route new requests to another instance while one is
busy. Break a long computation up as
[running time](resource-limits.md#running-time) describes.
:::

`Lazy<T>` and the `Concurrent*` collections work unchanged.

### Use async, not blocking waits {#blocking-waits}

If you already write async code, there is nothing to do here. Waiting is
asynchronous, so a waiting instance occupies no thread and spends no running
time. A synchronous wait that would block throws
`PlatformNotSupportedException` immediately, whatever its timeout. Nothing
else on the thread could ever satisfy it, and it would hold the thread your
other requests need, so we fail fast rather than let it hang until the
timeout.

A wait that can be satisfied immediately succeeds, and a zero timeout returns
`false`. Everything else needs its async form:

| Call                                                                                            | Throws                                                       | Use instead                                                                             |
| ----------------------------------------------------------------------------------------------- | ------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| `task.Wait()`, `task.Result`                                                                    | when the task has not finished                               | `await task`                                                                            |
| `Task.WaitAll(...)`                                                                             | when a task has not finished                                 | `await Task.WhenAll(...)`                                                               |
| `Task.WaitAny(...)`                                                                             | when no task has finished                                    | `await Task.WhenAny(...)`                                                               |
| `semaphore.Wait()` on a `SemaphoreSlim`                                                         | when no slot is free                                         | `await semaphore.WaitAsync()`                                                           |
| `Wait()` on a `ManualResetEventSlim` or a `CountdownEvent`                                      | when the event is not set, or the count has not reached zero | `await` a `TaskCompletionSource`                                                        |
| `WaitOne()`, `WaitHandle.WaitAny(...)` and `WaitHandle.WaitAll(...)` on events or a `Semaphore` | when the handles are not signalled                           | `await` a `TaskCompletionSource`, or `await semaphore.WaitAsync()` on a `SemaphoreSlim` |
| `Take()` and `Add()` on a `BlockingCollection`                                                  | when it is empty, or full                                    | `await` `ReadAsync` and `WriteAsync` on a `Channel`                                     |
| `barrier.SignalAndWait()`                                                                       | unless this is the last participant to arrive                | —                                                                                       |
| `SpinWait.SpinUntil(...)`                                                                       | when the condition is false                                  | `await` the work the condition waits for                                                |
| `Monitor.Wait(...)`                                                                             | always, except with a timeout of zero                        | `await` a `TaskCompletionSource`                                                        |
| `Thread.Sleep(...)`                                                                             | always, except `Thread.Sleep(0)`                             | `await Task.Delay(...)`                                                                 |
| `Thread.CurrentThread.Join(...)`                                                                | always, except with a timeout of zero                        | —                                                                                       |

:::tip
`app.Run()` is rejected at build time.
[Adapt `Program.cs`](../how-to/onboard-an-application.md#adapt-programcs)
shows the replacement.
:::

Synchronous network calls and the synchronous APIs of libraries follow the
same rule. On an EF Core context, call `SaveChangesAsync` and `ToListAsync`,
not `SaveChanges` and `ToList`. Some blocking calls don't look like I/O:

| Call                                                                       | Why it blocks                                                                                                                | Use instead                                                 |
| -------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| ASP.NET Core's `Request.Form`, before the form has been read               | It reads the request body                                                                                                    | `await Request.ReadFormAsync()`                             |
| `Read` on `Request.Body` and `Write` on `Response.Body`                    | They read and write the connection                                                                                           | `ReadAsync` and `WriteAsync`                                |
| Razor's `Html.Partial` and `Html.RenderPartial`                            | They wait for the partial view, which fails if the partial awaits anything                                                   | `<partial name="..." />`, or `await Html.PartialAsync(...)` |
| Entity Framework Core's `Add`, on an entity whose keys come from `UseHiLo` | It fetches a block of keys from the database, on the first call and whenever a block runs out, so it fails only occasionally | `await AddAsync(...)`                                       |
| `using` on a data reader that still has unread rows                        | Disposing it reads the remaining rows                                                                                        | `await using`                                               |

[Library compatibility](library-compatibility.md) lists libraries that block
inside calls that look asynchronous.

:::tip
The analyzer reports most of these calls as
[`ISOPLANE1003`](../reference/error-codes.md#isoplane1003).
[Find and fix blocking calls](../how-to/find-and-fix-blocking-calls.md) walks
through an existing application with it.
:::

### Locks {#locks}

Locks on Isoplane behave as they do on any other asynchronous .NET host,
such as Kestrel. A `lock` block can't contain an `await`, so it runs to
completion within one slice. With one thread, that means it is never
contended and is always acquired immediately. The same goes for `Monitor`,
`Mutex`, `ReaderWriterLockSlim` and `System.Threading.Lock` used the same way.

On Isoplane, holding a thread-owned lock, such as a lock acquired manually via
`Monitor.Enter`, across an `await` triggers the same behaviour as any other
async host, if that host were configured to process all requests on a single
worker thread. All work runs on the same thread, so reentrant locks let the
second dispatch in, and non-reentrant ones throw `LockRecursionException`. This
only differs from multi-threaded hosts in that on a multi-threaded host these
locks fail unpredictably, either by silently re-entering, throwing, or
deadlocking.

A `SpinLock` without owner tracking throws `PlatformNotSupportedException`
rather than spinning forever.

:::tip
For mutual exclusion across an `await`, use `SemaphoreSlim` with
`await WaitAsync()`, and give it a timeout or the request's cancellation
token.
:::

### The clock {#clock}

To protect your application from its neighbours on the same hardware, we
withhold the precise clock that Spectre-style side-channel attacks depend on.
The clock you see is set at the start of each slice and stands still while
your code runs. Every reading within the slice, from `DateTime.UtcNow`,
`Stopwatch` or `Environment.TickCount64`, returns the same value, to the
millisecond, whatever `Stopwatch.IsHighResolution` claims. Timers have the
same resolution, and timers and cancellation tokens follow the same rule, so
one that fires during a slice is observed in the next.

What this means in practice:

- Across an `await` that suspends, `Stopwatch` measures from one slice start
  to the next, so it is an upper bound on the wait.
- Timestamps taken in one slice are identical, so log lines can share a time.
- A loop that polls its own deadline without awaiting never sees it arrive.
  Split the work and await between the parts.
- `Task.Delay`, `CancelAfter` and `Timer` callbacks fire between slices, never
  in the middle of a computation.

:::note
`Stopwatch` can't time computation within a slice because the clock is frozen.
:::

## Background work {#background-work}

Work still pending when a response is sent, such as an open connection, a timer
or a task you didn't await, becomes the instance's background work. An
instance's background work is limited by the instance's
[background allowance](resource-limits.md#background-allowance).

Timers, continuations and `BackgroundService` loops all run this way,
including the timers a library runs for itself, such as a connection pool's.
Nothing opts in. This is what lets the Isoplane OpenTelemetry exporter or a
Redis client finish what it started during a request. It runs only while its
instance exists, and an idle instance is evicted five minutes after its last request,
so a `BackgroundService` stops where it was, without its stopping token being
cancelled.

:::important[Background work is not guaranteed to run]
Background work is not guaranteed to run on time, or at all. Finish anything
the response depends on before you send it, and put work that must happen on
a durable queue outside the instance.
:::

:::tip
For work that has to run continuously or on a schedule, expose an endpoint
and have your scheduler or queue service call it.
[Run work on a schedule](../how-to/run-work-on-a-schedule.md) shows how.
:::

## Cleanup {#cleanup}

Most cleanup work, like `finally` blocks and automatic disposal at the end of
a `using` or `await using` block, works as you'd normally expect. However,
because each instance is single-threaded and finalizers run during a garbage
collection, a finalizer may run much later, in some other request, or never
if the instance is evicted first.

When an instance is evicted, for example as a result of exceeding an
application [limit](resource-limits.md), the instance is killed immediately,
so these automatic cleanup operations will not execute. If your application
terminates requests promptly upon [cancellation](#running-time), evictions
during request processing will be rare.

:::tip
Use `Dispose` for anything that matters. Don't rely on a finalizer.
:::

## Instance state {#instance-state}

### Instances are ephemeral {#ephemeral-instances}

We run your application as a set of ephemeral instances, adding them as
traffic grows and *evicting* them, which discards everything in their memory,
when they are idle or memory is needed. An instance can be evicted at any
time, although we avoid evicting one while it is handling a request where we
can. An instance that outgrows its memory limit, for example, takes no new
requests and is replaced once the ones it has are done. If your application
already runs as more than one replica behind a load balancer, nothing here is
new.

:::important[Instance memory is a cache]
Static fields, `IMemoryCache` and in-memory session state can vanish at any
time. Keep anything that must last in a database or another external service.
:::

### Initialization state snapshots {#initialization}

When a new template is needed, preparation runs startup code and captures
its memory. Initialization may retry, and matching captured state may be reused. In an ASP.NET Core application that is `Program.cs` up to `RunAsync`.
Every instance starts from that snapshot, the *template*, so instances reuse captured startup work instead of each rerunning startup. That is what lets a new instance answer
its first request without a cold start, and why expensive startup work, such
as building caches or warming routes, belongs in initialization rather than in
the first request.
[The application lifecycle](application-lifecycle.md#initialization) walks
through what initialization may and may not do.

Everything initialization leaves in memory is in the snapshot, including
whatever it read from configuration or generated, so it is identical in every
instance. The snapshot can contain secrets, and we treat it as one. Changing a
setting runs initialization again and makes a new snapshot, so no instance
runs with settings older than its snapshot. Two things follow:

- A value created during initialization is captured in the template and
  shared by its instances. Fresh initialization may create a different value;
  reusing a prepared template preserves its captured value. Keep long-lived
  values in configuration or external storage. For ASP.NET Core Data Protection,
  retain earlier keys while cookies or antiforgery tokens protected with them
  must remain readable.
  [Keep ASP.NET Core Data Protection keys across builds](../how-to/keep-data-protection-keys.md)
  shows how.
- A value that must be unique to each instance, such as an identifier or a
  nonce, has to be created after the snapshot. `Random.Shared`, unseeded
  `Random` instances and SQLite's `random()` are reseeded for you, and
  cryptographic randomness is always fresh. A seeded `Random` or a value
  already drawn during initialization is not.

:::tip
Create per-instance values in an `Activation.Activated` callback, which runs
when the instance starts.
[Activation](application-lifecycle.md#activation) shows one.
:::

## I/O and the system

### Incoming requests {#incoming-requests}

Your application receives HTTP requests routed to a free instance, so there is
no Kestrel to configure, and an application can't listen on a port itself.
The host serves plaintext HTTP; configure a reverse proxy to terminate TLS.

Requests arrive as HTTP/1.1, without upgrades or trailers. That rules out gRPC
over HTTP/2, which needs both, although gRPC-Web works. WebSockets are
currently unavailable, and with them Blazor Server interactivity. SignalR is
[not supported](library-compatibility.md#signalr) on any of its transports.

Streamed responses work, so server-sent events from your own endpoint do too,
as do statically rendered Blazor pages. See
[Check application assumptions](../how-to/onboard-an-application.md#check-application-assumptions).

### Outgoing connections {#outgoing-connections}

An application can open TCP connections through the async APIs, which covers
`HttpClient`, TLS and database drivers. There is no UDP. Outbound connections
are subject to a per-instance
[outbound connection limit](resource-limits.md#outbound-connections).

In-process connection pools aren't shared across instances, so we recommend
an external connection pooler to stay within your database provider's
connection limits. [Connect to PostgreSQL](../how-to/connect-postgresql.md)
turns the driver's own pool off for that reason.

:::tip
Where a library offers a choice of transport, such as a metrics or logging
client that can also send over UDP, pick TCP or HTTP.
:::

:::important
Close every connection before initialization finishes, or the application
refuses to start. See [initialization](application-lifecycle.md#initialization)
for more info.
:::

### Socket options

Socket options are clamped to our ranges, and reading one back returns the
value in force:

| Option                        | Range                                                      |
| ----------------------------- | ---------------------------------------------------------- |
| Receive and send buffer sizes | 4,096 to 212,992 bytes. An unset size reads back as 65,536 |
| Keep-alive idle time          | 10 to 32,767 seconds                                       |
| Keep-alive interval           | 1 to 32,767 seconds                                        |
| Keep-alive probe count        | 1 to 127                                                   |

`Socket.Ttl` throws `SocketException` with `OperationNotSupported`.

### HTTP clients {#http-clients}

`IHttpClientFactory` and `new HttpClient()` pool connections as usual, and
we never close an idle connection ourselves.

During [initialization](application-lifecycle.md#initialization), factory
requests send `Connection: close` so that warm-up calls leave no open sockets.
A custom handler that ignores that needs its pooling disabled during
initialization.

### File system {#file-system}

The file system is read-only, and holds only your application package:
`appsettings.json`, `wwwroot` and so on. Writes fail, and there is currently
nowhere to store temporary files.

A file's modification time is the time it was last written before the
application was built, which ASP.NET Core sends as `Last-Modified` when
serving static files.
[Build the same package on any machine](../how-to/run-and-rebuild.md#build-the-same-package-on-any-machine)
says how to make it the same on every build.

:::tip
[Handle uploads and generated files](../how-to/handle-uploads-and-generated-files.md)
shows how to work without a writable file system.
:::

### Environment variables and processes {#environment-and-processes}

Your settings arrive through `IConfiguration`, as they already do from
`appsettings.json` and user secrets. Environment variables are empty. There
are no other processes, and you can't start one.

:::tip
[Local development settings](../how-to/local-settings.md) shows where settings
come from while you develop. Replace a child process with a managed library,
or call a service that does the work.
:::

### Native libraries {#native-libraries}

We ship .NET's own native components (compression, globalization, OpenSSL) and
[SQLite](library-compatibility.md#sqlite) for you, compiled to WebAssembly.
Your package ships managed code only. Native binaries from NuGet packages
aren't loaded, and a P/Invoke into one fails when called, not at build or
startup. A package that ships native code still works as long as you don't
attempt to call native code.

:::tip
The analyzer can report native-import declarations as
[`ISOPLANE1001`](../reference/error-codes.md#isoplane1001).
[Control compatibility warnings](../how-to/run-and-rebuild.md#control-compatibility-warnings)
shows how to switch that on.
:::

### Culture and time zone {#culture-and-time-zone}

Local time is UTC and the default culture is invariant, because an instance
isn't tied to a machine with a locale of its own. Named cultures and time
zones are all available, and
[ASP.NET Core's globalization and localization guide](https://learn.microsoft.com/aspnet/core/fundamentals/localization)
applies unchanged.

## Where to go next

- Bringing an existing application? Work through the checklist in
  [Onboard an application](../how-to/onboard-an-application.md).
- Starting fresh? The [project templates](../reference/project-templates.md)
  already follow everything on this page.
- Looking for the numbers? See [resource limits](resource-limits.md).
