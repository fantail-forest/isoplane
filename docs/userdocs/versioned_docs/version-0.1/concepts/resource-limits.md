---
title: Resource limits
---

We bound what one instance and one
[dispatch](dotnet-runtime-differences.md#dispatches-and-slices) can use, so
that no neighbour on the same hardware can take resources from your
application, and so that one runaway request can't take down the rest of it.
A typical request stays well inside every limit on this page.

:::note[Preliminary limits]
These defaults match the preview SDK. They may change in later releases.
:::

## The limits {#local-defaults}

| Limit | Applies to | Current limit | At the limit |
| --- | --- | --- | --- |
| Live instances | Application | 16 | A request that needs a new instance gets `503` with `Retry-After: 1` |
| Active requests | Instance | 8 | Further requests go to another instance |
| Memory | Instance | 512 MiB | The instance takes no new requests and is replaced once its current ones finish |
| Running time | Request | 300 s | The request's cancellation token fires, then the request has 5 s to finish |
| Background running time | Instance | 5 s to start, refilling to 30 s | Background work waits until the allowance refills |
| Outbound connections | Instance | 32 open, 6 opening at once | Opening another connection fails with a socket error |
| Pending operations | Request | 4096 | Further operations wait their turn |
| Application package | Application | 50 MiB uncompressed | Preparing the application fails |

To see how your application behaves under smaller limits, see
[Try the application under tighter limits](../how-to/local-settings.md#try-the-application-under-tighter-limits).

## Instances and requests

Each application can have at most 16 instances active, and each instance can
process up to eight requests at once, allowing for up to 128 requests to be
processed concurrently per application at any given time. A request goes to an
instance with a free slot, and we start another instance when none has one. An
instance that is finishing cancelled requests, or draining after a new build,
still counts. A streaming response holds its slot until the body is complete.

When every instance is full and the application is at its instance limit, the
request is refused with `503` and `Retry-After: 1` rather than queued.

## Memory {#memory}

Memory limits are applied per instance, measured as the total linear memory
allocated by that instance. An application whose initialization attempts to
allocate more than the memory limit can't start.

If an instance reaches the limit, we stop sending it requests, let it finish
the ones it has, and replace it. While it finishes it may grow up to 125% of
the limit. Once 125% of the limit is reached, further allocations may result in
an `OutOfMemoryException` or a hard kill of the instance. If the instance is
killed, all requests that it's currently processing will be terminated early.


`GC.GetGCMemoryInfo().TotalAvailableMemoryBytes` reports the limit. Garbage
collection should keep managed memory usage within the limit, but it cannot
account for native memory use, so it's possible that applications with heavy
usage of native code (e.g. outbound TLS connections) may exceed the memory
limit.

:::tip
Read and write large bodies as streams, so they never sit in memory.
[Handle uploads and generated files](../how-to/handle-uploads-and-generated-files.md)
shows how.
:::

## Running time {#running-time}


[Running time](dotnet-runtime-differences.md#running-time), is the amount of
time that an instance of your application occupies a worker thread, plus any
time that the host spends in I/O threads handling I/O on behalf of the
instance. This includes time a worker is descheduled, but excludes parked
asynchronous waits.

There is no wall-clock request deadline while the client stays connected, so a
slow downstream call or a long streamed response is fine.

When the allowance runs out, or the client disconnects, the request's
cancellation token fires. That token is `HttpContext.RequestAborted`, or
[`RequestHead.Aborted`](../reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
without ASP.NET. The request then has 5 seconds by the clock, waiting
included, to finish. A request that finishes in time, whether with a response
or an exception, leaves its instance as it was. One that is still running when
the 5 seconds are up is stopped, and its instance with it, so every other
request on that instance fails too. We send no new requests to an instance
while one of its requests is finishing after cancellation, so only the
requests already there are at risk.

Cancellation reaches your code only at an `await`. Like the
[clock](dotnet-runtime-differences.md#clock), the token stands still while
code runs, so a loop that checks `IsCancellationRequested` without awaiting
never sees it change.

:::tip
Pass the token to everything you await, give any wait on an asynchronous lock
a timeout or the token, and let `OperationCanceledException` end the request.
Split a long computation into parts, `await Task.Yield()` between them, and
check the token after each yield.
:::

## Background running time {#background-allowance}

Work still pending when a response is sent becomes the instance's [background
work](dotnet-runtime-differences.md#background-work) and runs on a separate
running time allowance from requests. Background running time is metered in the
same way as request running time. Idle periods and time spent suspended on an
`await` don't count toward the limit.

An instance starts with 5 seconds of background running time and earns an
additional 5 seconds for every 10 seconds that pass. Requests that finish
before their running time limit is exhausted donate the unused portion of their
allowance to the background running time allowance pool. The background running
time allowance pool is capped at 30 seconds.

Background work waits whenever the allowance is below 5 seconds, and a request
that is waiting on shared background work, such as a connection a client
library keeps open, waits with it. A library timeout can therefore be reported
late.

Background work doesn't keep an instance alive. An instance with no requests
for five minutes is evicted, whatever it still holds.

## Outbound connections {#outbound-connections}

Each instance has a limit on concurrent outbound connections. A connection
counts from the moment it starts opening until your application closes or
disposes it. This limit applies to active connections as well as idle
connections in connection pools.

Attempting to open additional connections when the limit is exhausted fails
with a socket error. Up to six connections can be opening at the same time, and
further ones wait their turn rather than failing. Other instances, including
those of the same application, don't count against an instance's limit.

:::tip
Set `concurrentConnectionsPerInstance` in
[local settings](../how-to/local-settings.md) to test your application under a
lower limit.
:::

## Socket options {#socket-options}

Socket options are clamped to these ranges, and reading an option back
returns the value in force:

| Option                        | Range                                                      |
| ----------------------------- | ---------------------------------------------------------- |
| Receive and send buffer sizes | 4,096 to 212,992 bytes. An unset size reads back as 65,536 |
| Keep-alive idle time          | 10 to 32,767 seconds                                       |
| Keep-alive interval           | 1 to 32,767 seconds                                        |
| Keep-alive probe count        | 1 to 127                                                   |

`Socket.Ttl` throws `SocketException` with `OperationNotSupported`.

## Pending operations

Each request has a cap on the host operations it can have in flight at once,
such as outbound calls, DNS lookups, timers, and reads and writes of the
request and response bodies. Past the cap, further operations wait their turn
and start, in the order you started them, as earlier ones complete. A request
that fans out thousands of calls takes longer, and nothing fails. Background
work has a cap of its own.

## Where to go next

- Trying your application under smaller limits?
  [Try the application under tighter limits](../how-to/local-settings.md#try-the-application-under-tighter-limits).
- Wondering why time and memory behave this way?
  [How .NET differs on Isoplane](dotnet-runtime-differences.md).
- Need work to run outside a request?
  [Run work on a schedule](../how-to/run-work-on-a-schedule.md).
