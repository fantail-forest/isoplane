---
title: How .NET differs on Isoplane
---

An Isoplane application is ordinary .NET code that the Isoplane engine runs
in a WebAssembly sandbox rather than as a process of its own. This page
covers the runtime behaviour that differs from the same code running as an
ordinary .NET process, what each difference means for your code, and what to
do about it. Behaviour the page does not mention is unchanged. If your
application already awaits its I/O, expect a few changes, mostly to state
kept in memory, to code that waits synchronously, and to background work.

Three facts about the sandbox explain the differences:

1. **The application runs as ephemeral instances.** Every instance is a
   [copy-on-write](https://en.wikipedia.org/wiki/Copy-on-write) clone of the
   *template*, which is the application's memory saved once its startup code
   has run.
2. **Isoplane enforces [cooperative
   concurrency](https://en.wikipedia.org/wiki/Cooperative_multitasking).**
   An instance's code runs on at most one thread at a time, and occupies that
   thread until it suspends on an `await`. Only then can the thread run other
   code, whether in the same instance or in another instance. The engine
   [stops a request that runs for too long](#running-time) and
   [prevents blocking operations](#blocking-waits), so instances do not rely
   on one another to yield.
3. **All I/O is
   [mediated](https://en.wikipedia.org/wiki/Capability-based_security) by the
   Isoplane engine.** The engine acts as a [reference
   monitor](https://en.wikipedia.org/wiki/Reference_monitor) for the instance.
   Your code uses the ordinary .NET APIs; underneath, every network, file,
   clock and operating-system call is a call into the engine, which performs
   it, accounts for it, and can refuse it.

## Instances and the template {#instance-state}

### Every instance starts from a template {#initialization}

When an application is deployed, the engine runs its startup code and its
initialization callbacks, together called *template initialization*, and then
saves the application's memory, all before serving any request. That saved
memory is the *template*, and every instance starts as a copy of it. A fresh
.NET process would spend its first seconds on startup work and on the
one-time work its first requests trigger. A new instance skips all of that,
because it begins where template initialization left off. Expensive setup,
such as building caches or warming up code paths, therefore belongs in
template initialization, not in the first request. For what initialization
may and may not do, see
[the application lifecycle](application-lifecycle.md#initialization).

Every instance starts with identical memory, down to any random values
initialization generated. Changing a setting makes the engine run
initialization again and save a new template. Initialization may also run
more than once before a template is saved, so initialization callbacks need
to be safe to repeat.

Initialization must also close every connection and stream it opens, because
an open connection cannot be saved in the template. The engine refuses to
save a template while a connection is open.

Because the template is identical in every instance, anything that has to
differ between instances, such as an instance identifier or a nonce, has to
be written in an *instance activation* callback, which runs in each instance
before its first request. A value initialization already generated can be
overwritten there. The engine reseeds the framework's random number
generators at the same point. For which generators, and an example callback,
see [instance activation](application-lifecycle.md#activation).

### Instances come and go {#ephemeral-instances}

The engine adds instances as traffic grows and discards them when they are no
longer needed. When it discards an instance, it normally waits until no
request is running on it. The exceptions are an instance that [ignores
cancellation](#running-time) or overruns its memory limit, which the engine
stops while requests are still running. For the memory limit, see
[Resource limits](resource-limits.md#memory).

Successive requests from one client can reach different instances, and
nothing ties a client to an instance. If your application already runs as
several processes behind a load balancer, this is familiar; the difference is
that the engine decides how many instances exist and when each instance starts
and stops.

### Disposal and finalizers {#cleanup}

`finally` blocks, `using` and `await using` run as expected, except when an
instance is discarded while a dispatch is still running. Finalizers may not
run at all, because a finalizer runs only after a garbage collection and an
instance can be discarded before a collection happens. When the engine
discards an instance, nothing in it runs again: no `finally` block, no
`Dispose`, no finalizer. Dispose what you open before the response is sent,
and do not rely on a finalizer.

## Cooperative concurrency {#cooperative-concurrency}

### Dispatches and slices {#dispatches-and-slices}

A *dispatch* is one unit of work the engine gives an instance. The engine
starts a dispatch when a request arrives, and when background work such as a
timer callback is due. A *slice* is one uninterrupted run of a dispatch's
code, from when it starts or resumes until it awaits something or finishes.
No two slices run at the same time within an instance.

<svg viewBox="0 0 720 175" role="img" aria-label="Three dispatches on one instance, all started by requests. Each row is one dispatch, from the request's arrival to its response, made up of slices, waits on operations, and time spent ready while another slice is running" style={{width: '100%', maxWidth: '720px', display: 'block', margin: '1rem 0'}} fontFamily="var(--ifm-font-family-base, sans-serif)" fontSize="13">
  <text x="90" y="44" textAnchor="end" fill="currentColor">Dispatch 1</text>
  <rect x="100.0" y="26" width="591.4" height="28" rx="6" fill="currentColor" opacity="0.08" />
  <rect x="100.0" y="29" width="68.6" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="168.6" y1="40" x2="220.0" y2="40" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="221.0" y="30" width="32.3" height="20" rx="4" fill="none" stroke="var(--ifm-color-primary, #3b7a57)" strokeWidth="1.5" />
  <rect x="254.3" y="29" width="68.6" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="322.9" y1="40" x2="425.7" y2="40" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="426.7" y="30" width="40.9" height="20" rx="4" fill="none" stroke="var(--ifm-color-primary, #3b7a57)" strokeWidth="1.5" />
  <rect x="468.6" y="29" width="42.9" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="511.4" y1="40" x2="631.4" y2="40" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="632.4" y="30" width="15.1" height="20" rx="4" fill="none" stroke="var(--ifm-color-primary, #3b7a57)" strokeWidth="1.5" />
  <rect x="648.6" y="29" width="42.9" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <text x="90" y="84" textAnchor="end" fill="currentColor">Dispatch 2</text>
  <rect x="125.7" y="66" width="522.9" height="28" rx="6" fill="currentColor" opacity="0.08" />
  <rect x="126.7" y="70" width="40.9" height="20" rx="4" fill="none" stroke="var(--ifm-color-primary, #3b7a57)" strokeWidth="1.5" />
  <rect x="168.6" y="69" width="34.3" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="202.9" y1="80" x2="400.0" y2="80" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="400.0" y="69" width="68.6" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="468.6" y1="80" x2="622.9" y2="80" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="622.9" y="69" width="25.7" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <text x="90" y="124" textAnchor="end" fill="currentColor">Dispatch 3</text>
  <rect x="134.3" y="106" width="462.9" height="28" rx="6" fill="currentColor" opacity="0.08" />
  <rect x="135.3" y="110" width="66.6" height="20" rx="4" fill="none" stroke="var(--ifm-color-primary, #3b7a57)" strokeWidth="1.5" />
  <rect x="202.9" y="109" width="51.4" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="254.3" y1="120" x2="271.4" y2="120" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="272.4" y="110" width="49.4" height="20" rx="4" fill="none" stroke="var(--ifm-color-primary, #3b7a57)" strokeWidth="1.5" />
  <rect x="322.9" y="109" width="25.7" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="348.6" y1="120" x2="494.3" y2="120" stroke="currentColor" strokeWidth="1.5" strokeDasharray="4 4" opacity="0.4" />
  <rect x="495.3" y="110" width="15.1" height="20" rx="4" fill="none" stroke="var(--ifm-color-primary, #3b7a57)" strokeWidth="1.5" />
  <rect x="511.4" y="109" width="85.7" height="22" rx="4" fill="var(--ifm-color-primary, #3b7a57)" />
  <line x1="100" y1="154" x2="700.0" y2="154" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <line x1="100.0" y1="154" x2="100.0" y2="159" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="100.0" y="173" textAnchor="middle" fill="currentColor" opacity="0.8">0 ms</text>
  <line x1="185.7" y1="154" x2="185.7" y2="159" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="185.7" y="173" textAnchor="middle" fill="currentColor" opacity="0.8">50</text>
  <line x1="271.4" y1="154" x2="271.4" y2="159" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="271.4" y="173" textAnchor="middle" fill="currentColor" opacity="0.8">100</text>
  <line x1="357.1" y1="154" x2="357.1" y2="159" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="357.1" y="173" textAnchor="middle" fill="currentColor" opacity="0.8">150</text>
  <line x1="442.9" y1="154" x2="442.9" y2="159" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="442.9" y="173" textAnchor="middle" fill="currentColor" opacity="0.8">200</text>
  <line x1="528.6" y1="154" x2="528.6" y2="159" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="528.6" y="173" textAnchor="middle" fill="currentColor" opacity="0.8">250</text>
  <line x1="614.3" y1="154" x2="614.3" y2="159" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="614.3" y="173" textAnchor="middle" fill="currentColor" opacity="0.8">300</text>
  <line x1="700.0" y1="154" x2="700.0" y2="159" stroke="currentColor" strokeWidth="1" opacity="0.5" />
  <text x="700.0" y="173" textAnchor="middle" fill="currentColor" opacity="0.8">350</text>
</svg>

Each row is one dispatch, started by a request. The pale band runs from the
request's arrival to its response. Within the band, a solid bar is a slice, a
dashed line is a wait on an operation, and a hollow bar is time spent ready
while another slice is running. A dispatch becomes ready when the operation it
awaited completes, and runs when the thread is free, so slices run in whatever
order dispatches become ready. Where no row has a solid bar, the instance
occupies no thread.

The engine runs slices on a pool of *execution threads* and performs the
instances' I/O on a separate pool of *reactor threads*. When an operation
completes on a reactor thread, the engine adds the continuation it resolved to
the execution pool's queue. The next slice begins when a free execution thread
pops that continuation from the queue.

The new slice belongs to the dispatch that owned the operation if that
dispatch is still active; if the dispatch has finished, the slice runs as a
new dispatch of [background work](#background-work), which counts against the
instance's [background allowance](resource-limits.md#background-allowance).

An instance occupies an execution thread only while a slice is running, which
is how the engine serves far more instances than it has threads. If you know
Node.js, this is the shape of V8 and libuv, with application code on one
thread per instance, I/O in parallel on a pool, and completions queued back to
the application thread.

### One thread per instance {#one-thread-per-instance}

The sandbox excludes WebAssembly's shared memory and atomics, so only one
thread can ever execute inside an instance. `new Thread(...).Start()`
therefore throws `PlatformNotSupportedException`, and
`Environment.ProcessorCount` is 1.

`Task.Run`, `ThreadPool.QueueUserWorkItem` and `Parallel.For` do not throw.
They run on the instance's one thread, so queued work runs the next time your
code awaits, and parallel loops run their iterations one after another. They
lose their speed-up, but they do not need removing.

Even though an instance's own code executes serially, the engine runs multiple
I/O operations from the same instance in parallel on the reactor thread pool.
For the best I/O throughput, start several asynchronous operations without
awaiting them, then await them together with `Task.WhenAll` or `Task.WhenAny`.
To keep a long computation from blocking the instance, see [Running time and
cancellation](#running-time).

### Blocking waits and synchronous I/O {#blocking-waits}

Isoplane is designed to support extremely dense, oversubscribed deployments,
while offering response latencies that are as good as, if not better than, all
other current .NET hosting options that offer scale-to-zero. The initial
engineering goal is to support 1,000 instances running typical .NET API
workloads on a single 16-core host machine with 64 GiB of system memory.

This goal hinges on a simple rule: no instance may perform any synchronous
blocking I/O or wait operations. A blocked instance would occupy an execution
thread without doing any work, and the host machine has far fewer execution
threads than instances. The .NET runtime enforces this rule by throwing
`PlatformNotSupportedException` any time a call would result in a synchronous
blocking wait, including calls made inside an `async` method.

This means that your code must call the asynchronous form of any operation that
would ordinarily block, and must `await` the result wherever it needs a wait or
an I/O operation to finish before it continues.

It's important to note that the runtime only throws in cases where proceeding
with execution would *actually* result in a blocking wait. If a method or
operation only sometimes results in a blocking wait, the runtime won't throw in
the cases when no blocking wait would occur. For example, the runtime won't
throw on calls to a synchronous method that accepts a timeout argument if the
timeout is set to `0`, provided that the implementation of that method returns
immediately rather than blocking when a zero timeout is specified.

The Roslyn analyzer that ships with Isoplane reports potentially blocking waits
as [`ISOPLANE1003`](../reference/error-codes.md#isoplane1003). See [Find and
fix blocking calls](../how-to/find-and-fix-blocking-calls.md) for an example.

See the table below for a list of common runtime operations that may trigger a
`PlatformNotSupportedException` due to this rule, and how to keep that from
happening:

| Call                                                                                            | Throws                                                       | Use instead                                                                             |
| ----------------------------------------------------------------------------------------------- | ------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| `task.Wait()`, `task.Result`                                                                    | when the task has not finished                               | `await task`                                                                            |
| `Task.WaitAll(...)`                                                                             | when a task has not finished                                 | `await Task.WhenAll(...)`                                                               |
| `Task.WaitAny(...)`                                                                             | when no task has finished                                    | `await Task.WhenAny(...)`                                                               |
| `semaphore.Wait()` on a `SemaphoreSlim`                                                         | when no slot is free                                         | `await semaphore.WaitAsync()`                                                           |
| `Wait()` on a `ManualResetEventSlim` or a `CountdownEvent`                                      | when the event is not set, or the count has not reached zero | `await` a `TaskCompletionSource`                                                        |
| `WaitOne()`, `WaitHandle.WaitAny(...)` and `WaitHandle.WaitAll(...)` on events or a `Semaphore` | when the handles are not signalled                           | `await` a `TaskCompletionSource`, or `await semaphore.WaitAsync()` on a `SemaphoreSlim` |
| `Take()` and `Add()` on a `BlockingCollection`                                                  | when it is empty, or full                                    | `await` `ReadAsync` and `WriteAsync` on a `Channel`                                     |
| `barrier.SignalAndWait()`                                                                       | unless this is the last participant to arrive                | no replacement; remove the call                                                         |
| `SpinWait.SpinUntil(...)`                                                                       | when the condition is false                                  | `await` the work the condition waits for                                                |
| `Monitor.Wait(...)`                                                                             | always, except with a timeout of zero                        | `await` a `TaskCompletionSource`                                                        |
| `Thread.Sleep(...)`                                                                             | always, except `Thread.Sleep(0)`                             | `await Task.Delay(...)`                                                                 |
| `Thread.CurrentThread.Join(...)`                                                                | always, except with a timeout of zero                        | no replacement; remove the call                                                         |

### Locks {#locks}

A `lock` block cannot contain an `await`, so it always runs inside a single
slice. No other code runs during a slice, so the lock is never contended and
is always acquired at once. The same is true of `Monitor`, `Mutex`,
`ReaderWriterLockSlim` and `System.Threading.Lock` when they are used the
same way, around code that does not await, and `Lazy<T>` and the
`Concurrent*` collections work unchanged for the same reason.

A thread-owned lock held across an `await` protects nothing. A lock acquired
with `Monitor.Enter` and released after an `await` is owned by the thread, and
there is only one thread, so the next dispatch to reach the lock arrives on
the owning thread. A reentrant lock lets it in silently, and two dispatches
run inside the lock at once with nothing to report it. A non-reentrant lock
throws `LockRecursionException`.

A `SpinLock` created with `enableThreadOwnerTracking` off cannot tell that it
is already held, so re-entering it would spin forever; it throws
`PlatformNotSupportedException` instead.

For mutual exclusion across an `await`, use a `SemaphoreSlim` with
`await WaitAsync()`, and give the wait a timeout and the request's
cancellation token.

### Running time and cancellation {#running-time}

The engine meters each dispatch by its *running time*, the total duration of
its slices plus the time reactor threads spend processing its I/O. Running
time plays the role that CPU time plays for a process. It counts the time the
dispatch's code is executing, and not the time the dispatch spends waiting
between slices. Each request has a [running-time
allowance](resource-limits.md#running-time), and background work has a
[separate allowance](resource-limits.md#background-allowance).

Every request carries a cancellation token,
[`RequestHead.Aborted`](../reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
in the SDK, which a web framework exposes through its own request
abstraction. The engine cancels the token when the request's allowance runs
out and when the client disconnects, and the request then has a grace period
in which to finish.

:::important[Honour cancellation promptly]
If a cancelled request is still running when its grace period ends, the
engine [discards the whole instance](#ephemeral-instances), and every other
request on it fails too. Pass the token to everything you await, and let
`OperationCanceledException` end the request. For the allowance and the
grace period, see [Resource limits](resource-limits.md#running-time).
:::

Cancellation reaches your code only at an `await`. While a method computes
without awaiting, nothing else on the instance runs: not the token's
callbacks, and not the other requests. Split a long computation into parts,
and `await Task.Yield()` between them. A yield ends the slice, which gives the
instance's other requests a turn and lets a cancellation reach the token.
Check the token after each yield, for example with
`cancellationToken.ThrowIfCancellationRequested()`.

### Background work {#background-work}

*Background work* is any asynchronous task that runs on an instance after the
request that scheduled it has completed, or that no request scheduled, such as
a timer left running by template initialization.

The engine runs background work within the instance's
[background allowance](resource-limits.md#background-allowance), and only
while the instance exists.

:::important[Background work is not guaranteed to run]
It may run late, or not at all. Finish anything the response depends on
before sending the response.
:::

For work that must happen, use a durable queue outside the instance. For
work that must run continuously or on a schedule, expose an endpoint and
have a scheduler call it; see
[Run work on a schedule](../how-to/run-work-on-a-schedule.md).

## All I/O goes through the engine {#io}

Your application has no direct access to the network, the file system, the
clock or the operating system. The engine provides its own version of all
four, and the sections below describe how those versions differ from what an
ordinary process gets.

### Incoming requests {#incoming-requests}

The engine receives HTTP requests and routes each request to an instance with
available capacity. The application runs no web server of its own and cannot
listen on a port. The engine serves plain HTTP, and TLS is terminated in front
of it; see [Deploy to your own host](../how-to/deploy-an-application.md).

Requests arrive as HTTP/1.1, without connection upgrades or trailers, so
WebSockets are unavailable. Streamed responses work.

### Outgoing connections {#outgoing-connections}

An application can open TCP connections, with or without TLS, through the
asynchronous APIs: `HttpClient`, sockets and database drivers all work.
There is no UDP, so where a library offers a choice of transport, choose TCP
or HTTP.

Each instance has its own
[outbound connection limit](resource-limits.md#outbound-connections), and
each instance pools connections on its own. A pool size in your
configuration therefore caps one instance, not the application. The engine
never closes an idle connection on the application's behalf, so with many
instances the total can exceed what your database allows. Cap connections
with an external pooler, in front of the database. The
[Connect to PostgreSQL](../how-to/connect-postgresql.md) guide turns the
driver's own pool off for this reason.

The engine clamps socket buffer sizes and keep-alive settings to fixed
ranges, and reading an option back returns the value in force; for the
ranges, see [Resource limits](resource-limits.md#socket-options).
`Socket.Ttl` throws `SocketException` with `OperationNotSupported`.

### The clock {#clock}

The engine reads the clock when a slice starts and gives the instance that
one value for the whole slice. Every reading within a slice, from
`DateTime.UtcNow`, `Stopwatch` or `Environment.TickCount64`, returns the
same millisecond. Timers and cancellation tokens have the same resolution
and follow the same rule, so a timer that is due during a slice fires in the
next slice.

The clock is frozen deliberately. A fine-grained clock is what timing
side-channel attacks need, and the engine withholds such a clock from the
code it runs, including dependencies you did not write.

What this means for your code:

- `Stopwatch` cannot time a computation. Within a slice it always reads zero
  elapsed.
- Across an `await` that suspends, `Stopwatch` measures from the start of one
  slice to the start of the next, so it reports an upper bound on the wait.
- Timestamps taken in one slice are identical. Log lines written together
  share a time.
- A loop that polls its own deadline without awaiting never sees the deadline
  arrive. Split the work and await between the parts.
- `Task.Delay`, `CancelAfter` and `Timer` callbacks fire between slices, never
  in the middle of a computation.

### File system {#file-system}

The file system is read-only and holds only the application package: the
assemblies, settings files and static content the build produced. Writes fail,
and there is nowhere to put a temporary file. To work without a temporary
file, see [Handle uploads and generated
files](../how-to/handle-uploads-and-generated-files.md).

A file's modification time is the time it was last written before the
application was built. To make it identical from build to build, see [Build
the same package on any
machine](../how-to/run-and-rebuild.md#build-the-same-package-on-any-machine).

### Environment variables and processes {#environment-and-processes}

Settings reach the application through the SDK's
`RuntimeConfiguration.ReadValues()`, which a framework's configuration system
reads from, never through the environment. For where they come from while you
develop, see [Local development settings](../how-to/local-settings.md).

There are no other processes, and an application cannot start a process.
Replace a child process with a managed library, or with a call to a service
that does the work.

### Native libraries {#native-libraries}

The .NET runtime Isoplane ships includes .NET's own native components for
compression, globalization and cryptography, as well as
[SQLite](library-compatibility.md#sqlite), all compiled to WebAssembly. Your
application package ships managed code only. A native binary in a NuGet
package is never loaded, and a P/Invoke into such a binary fails when it is
called, not at build or startup. A package that ships native code still works
as long as the code paths you use never call it.

The analyzer can report native-import declarations as
[`ISOPLANE1001`](../reference/error-codes.md#isoplane1001). To switch that on,
see [Control compatibility
warnings](../how-to/run-and-rebuild.md#control-compatibility-warnings).

### Culture and time zone {#culture-and-time-zone}

Local time is UTC and the default culture is invariant, because an instance
is not tied to a machine with a locale of its own. Named cultures and time
zones are all available, and
[.NET's globalization guidance](https://learn.microsoft.com/dotnet/core/extensions/globalization)
applies unchanged.

## Where to go next

- Bringing an existing application? Work through the checklist in
  [Bring an existing ASP.NET application](../how-to/onboard-an-application.md).
- Starting fresh? The [project templates](../reference/project-templates.md)
  already follow everything on this page.
- Using a package that does one of the things above? See
  [Library compatibility](library-compatibility.md) for which packages fail
  and what to use instead.
- Looking for the numbers? See [Resource limits](resource-limits.md).
