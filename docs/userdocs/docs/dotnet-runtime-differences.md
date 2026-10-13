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

Isoplane runs many applications on a small number of worker threads. Whenever an
application is idle, because it is waiting on an asynchronous operation or has
no requests to process, Isoplane sets that instance aside and gives the worker
to another one.

Most of the rules below follow from this: waits are asynchronous, the
application has no threads of its own, and nothing runs between requests. The
rest follow from the sandbox itself: Isoplane mediates every interaction an
application has with the outside world, including the network, files, the clock
and waiting, so an application can do only what Isoplane provides.

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
- **A `lock` always succeeds.** With one thread there is never another holder,
  so `lock` and `Monitor.Enter` take the lock at once and never throw.
- **Blocking primitives throw every time.** `SemaphoreSlim.Wait` and
  `ManualResetEventSlim.Wait` throw `PlatformNotSupportedException` on every
  call, even when the semaphore has a free slot or the event is already set.
  `Monitor.Wait` with no timeout does the same. Use `SemaphoreSlim.WaitAsync`, or
  a `TaskCompletionSource` in place of an event.

## Waiting is asynchronous

A synchronous wait throws `PlatformNotSupportedException`. Some calls throw only
when they would actually have to wait, and others throw every time:

| Call | Throws | Use instead |
| --- | --- | --- |
| `task.Result` | when the task has not finished | `await task` |
| `task.Wait()` | unless the task has already completed successfully | `await task` |
| `Task.WaitAny(...)` | when no task has finished | `await Task.WhenAny(...)` |
| `Task.WaitAll(...)` | always | `await Task.WhenAll(...)` |
| `Thread.Sleep(...)` | always, except `Thread.Sleep(0)` | `await Task.Delay(...)` |
| `semaphore.Wait()` | always | `await semaphore.WaitAsync()` |

`app.Run()` is refused earlier, when the application builds: use
`await app.RunAsync()`.

The network behaves the same way. A synchronous socket call or name lookup that
would have to wait fails rather than waiting, so use the asynchronous methods.
A library's synchronous API usually blocks underneath: on an EF Core context,
call `SaveChangesAsync` and `ToListAsync`, not `SaveChanges` and `ToList`.

The build warns about several of these calls as
[`ISOPLANE1003`](reference/error-codes.md#isoplane1003), and the editor offers a quick fix
for most of them. It does not catch every case: `.Result` is not flagged, and
NuGet packages are not inspected unless you set
[`IsoplaneAnalyzeDependencies`](reference/project-settings.md#isoplaneanalyzedependencies).
The `app.Run()` error is described in
[Adapt `Program.cs`](onboard-an-application.md#adapt-programcs).

## Work belongs to a request

An application runs while it starts up and while it handles a request. Between
requests it does not run at all.

- **No background work.** A loop in a `BackgroundService`, a recurring timer or a
  queue consumer has nothing to run on. An application whose `BackgroundService`
  is still running once the host has started is refused, with an error that names
  the service. Bounded asynchronous work in `IHostedService.StartAsync` is
  accepted.
- **Await everything the request needs before it responds.** Once the response is
  complete the request is over. A timer or outbound call that is still pending is
  cancelled and never completes, so a task started and not awaited, such as
  `_ = SendEmailAsync()`, may never finish.
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
  reseeds `Random.Shared` and every `Random` created without a seed, and
  cryptographic randomness is always drawn fresh. A `Random` you gave a seed, a
  generator that is not a `System.Random`, and a value already drawn during
  initialization are copied as they are.
- **ASP.NET Core Data Protection keys are shared.** Every instance of a
  deployment protects and unprotects with the same key, so antiforgery tokens
  and authentication cookies work across instances.

[The application lifecycle](application-lifecycle.md) explains each of these in
more depth.

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
and dispose it when the request is done. The
[PostgreSQL sample](connect-postgresql.md) runs with the driver's own pooling
off.

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
- TLS certificates are validated as usual. Switching validation off is not a fix
  for a connection that fails.

## The machine underneath

- **The file system is read-only.** It holds the files of your application
  package, such as `appsettings.json` and `wwwroot`, and nothing else. Creating
  or writing a file fails, so there is nowhere to put temporary files. Keep uploads
  and generated files in memory while you handle the request, or in external
  storage.
- **Environment variables are empty.** Settings reach the application through
  configuration; see [local development settings](local-settings.md).
- **There are no other processes.** An application cannot start one.
- **Native libraries are not loaded.** A P/Invoke into a library the runtime does
  not include fails when it is called, not when the application builds or
  starts. A NuGet package that ships a native binary therefore still works as
  long as the code you use stays in managed code, and fails only on a path that
  reaches the native call. The build reports such declarations as
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
  `true`, and `Stopwatch.Frequency` still reports nanoseconds.

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
  it next waits, not in the middle of a computation.

## How code runs

Your code runs as IL on an interpreter. There is no JIT compiler, and nothing is
trimmed or compiled ahead of time, so reflection, runtime type loading,
`Reflection.Emit` and compiled expressions all work.

The SDK version in the project file, the number in
`Sdk="Isoplane.AspNetCore/..."`, also selects the runtime the application runs
on. Debug and Release run on that same runtime and differ only in how the C#
compiler optimizes your code.
