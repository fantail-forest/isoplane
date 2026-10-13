---
title: Runtime limitations
---

An Isoplane application is ordinary .NET code, but the sandbox it runs in is not
a server process, and some things a server lets you do are missing or behave
differently. This page lists all of them. Where another page covers one in more
depth, the entry links to it.

## Why the sandbox works this way

Isoplane runs many applications on a small number of worker threads. Whenever an
application is waiting, whether for a database reply or for its next request,
Isoplane sets that instance aside and gives the worker to another one.

It can only do that when the application has handed control back with no call in
progress. An `await` on unfinished work does exactly that: the method returns,
and what it needs to continue is kept in memory. A call that blocks does not,
because the wait happens inside the call.

Most of the rules below follow from this: waits are asynchronous, the
application has no threads of its own, and nothing runs between requests. The
rest follow from the sandbox itself, which gives an application the network and
its settings but not the machine underneath.

## One thread

Each instance has exactly one thread, and `Environment.ProcessorCount` is 1.

- **Starting a thread fails.** `Thread.Start` throws
  `PlatformNotSupportedException`. The build warns about it as `ISOPLANE1002`.
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
  a long computation does not see its request being cancelled.
- **A `lock` is never contended.** A wait that would have to block for another
  thread, such as `Monitor.Wait`, `SemaphoreSlim.Wait` or
  `ManualResetEventSlim.Wait`, throws instead.

## Waiting is asynchronous

Waiting synchronously for work that has not finished throws
`PlatformNotSupportedException`. Waiting on work that has already finished is
fine, so reading `.Result` from a completed task works.

| Instead of | Use |
| --- | --- |
| `Thread.Sleep(...)` | `await Task.Delay(...)` |
| `task.Wait()`, `task.Result` | `await task` |
| `Task.WaitAll(...)`, `Task.WaitAny(...)` | `await Task.WhenAll(...)`, `await Task.WhenAny(...)` |
| `semaphore.Wait()` | `await semaphore.WaitAsync()` |
| `app.Run()` | `await app.RunAsync()` |

The network behaves the same way. A synchronous socket call or name lookup that
would have to wait fails rather than waiting, so use the asynchronous methods.
A library's synchronous API usually blocks underneath: on an EF Core context,
call `SaveChangesAsync` and `ToListAsync`, not `SaveChanges` and `ToList`.

The build warns about several of these calls as `ISOPLANE1003`, and the editor
offers a quick fix for most of them. It does not catch every case: `.Result` is
not flagged, and NuGet packages are not inspected unless you ask.
[Control compatibility warnings](run-and-rebuild.md#control-compatibility-warnings)
covers both. `app.Run()` is an error, described in
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
- **Nothing is guaranteed to run after the response.** Code that is ready to run
  at that moment may still run, and so may finalizers, while the request is
  within its limits. Do not rely on either to save data or to release something.
- **Pass cancellation on.** `HttpContext.RequestAborted`, or
  [`RequestHead.Aborted`](reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
  without ASP.NET, is cancelled when the client goes away or the request uses up
  its running time.

## Instances come and go

Isoplane decides how many instances of an application run, and may discard one
after any request. Successive requests can reach different instances.

- **Memory is a cache, not storage.** Static fields, `IMemoryCache` and in-memory
  session state belong to one instance and can disappear. Keep anything that
  must last in a database or another external service.
- **Open connections may not survive between requests.** A stale connection is
  possible even after a request that completed normally.
- **A connection pool limit applies to one instance**, not to the application as
  a whole.
- **Startup state is captured and copied.** `Program.cs` runs up to `RunAsync` while
  Isoplane prepares the application, and every instance starts from a copy of
  that memory. A connection opened during startup cannot be carried into an
  instance, and a value created then is the same in every instance.

[The application lifecycle](application-lifecycle.md) explains each of these,
including what to create when an instance activates, how random number
generators are reseeded and what happens to ASP.NET Core Data Protection keys.

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
  not include fails when it is called, so a NuGet package that ships its own
  native binary will not work. The build can report such declarations as
  `ISOPLANE1001`; see
  [Control compatibility warnings](run-and-rebuild.md#control-compatibility-warnings).
- **The process is 32-bit.** `Environment.Is64BitProcess` is `false`.
- **Local time is UTC**, and the default culture is the invariant culture. Named
  cultures and time zones are available when you ask for them.
- **The clock moves only when the application waits.** Time does not advance
  during a stretch of pure computation, so a `Stopwatch` around code that never
  awaits can report no elapsed time.

## How code runs

Your code runs as IL on an interpreter. There is no JIT compiler, and nothing is
trimmed or compiled ahead of time, so reflection, runtime type loading,
`Reflection.Emit` and compiled expressions all work. See
[application packages and the shared runtime](execution-modes.md).

## Resource limits

Each instance has a memory limit and cannot grow past it.

Each request has an allowance of running time, which counts the time it spends
executing and not the time it spends waiting on something it awaits. When the
allowance runs out the request's cancellation token is cancelled; if the request
does not finish shortly afterwards, Isoplane stops the instance.

Each request can also have only so many asynchronous operations pending at once.
A request that goes over that number fails.

A local session runs with the largest limits the platform offers. To see how an
application behaves under smaller ones, see
[Try the application under tighter limits](local-settings.md#try-the-application-under-tighter-limits).
