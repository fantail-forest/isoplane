---
title: Execution limits
---

import LocalLimits from './_local-limits.mdx';

Isoplane limits what one instance and one
[dispatch](dotnet-runtime-differences.md#dispatches-and-slices) may use, so that no
application can take resources from the others running beside it. A dispatch is
a piece of work Isoplane gives an instance, usually one HTTP request to handle.
These limits are enforced by the platform. They are separate from the ways
[.NET itself behaves differently](dotnet-runtime-differences.md) in the sandbox.

## Application instances

Each application has a limit on live instances. Instances finishing cancelled
requests or draining an older application version still count, as do hibernated
instances. Requests can share an existing eligible instance; when another
instance is needed and usable capacity is exhausted, the request receives HTTP
503 with `Retry-After: 1`; it is not placed in an application admission queue.
An instance accepts at most eight active requests. An open streaming response
continues to occupy a request slot. The initial local limits are listed below.

## Memory

Each instance has a memory limit. The limit applies to the instance as a whole,
so whatever startup loaded and whatever an in-memory cache holds counts
alongside the requests being handled. An application whose startup alone leaves
more in memory than the limit cannot start.

An instance's allocated WebAssembly memory never shrinks. At the soft limit,
Isoplane stops adding concurrent requests to it. Once memory grows beyond that
limit, the instance retires: it finishes its existing requests and is replaced.
While it finishes, it may grow up to a
quarter past its limit and no further. Past that, an allocation fails with
`OutOfMemoryException`, or, if the .NET runtime itself cannot allocate, the
instance stops and every request it was handling fails.

`GC.GetGCMemoryInfo().TotalAvailableMemoryBytes` reports the limit, so caches
that trim themselves as memory runs short see it.

To keep a large request or response out of memory, read and write it as a
stream: [`StreamingRequest`](../reference/sdk/Isoplane.Sdk/Isoplane/StreamingRequest.md)
and [`Response.Stream`](../reference/sdk/Isoplane.Sdk/Isoplane/Response/Stream.md)
without ASP.NET, or the request and response body streams in an ASP.NET
application.

## Running time

Each request has an allowance of running time. Running time is the time the
dispatch spends executing. Time spent waiting for something it awaits, such as a
database or another service, does not count.

When the allowance runs out, the request's cancellation token is cancelled:
`HttpContext.RequestAborted`, or
[`RequestHead.Aborted`](../reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
without ASP.NET. The dispatch then has a short further period of running time in
which to finish. If it is still running at the end of it, Isoplane stops the
instance, `finally` blocks in the dispatch's code do not run, and every other
dispatch the instance was running fails too.

Cancellation reaches the application only when it awaits. Like the
[clock](dotnet-runtime-differences.md#clock), the token stands still while code
runs: `IsCancellationRequested` and `ThrowIfCancellationRequested` give the
answer from when the application last resumed, so a loop that checks them
without awaiting never sees the token cancel, and is stopped when the further
period ends. To stop a long computation cleanly, split it into parts and, between
them, await something that waits, such as `await Task.Delay(1, cancellationToken)`.
`await Task.Yield()` queues a continuation; it may still run in the same slice.
The runtime bounds callback batches, but a single callback must return before
that cooperative boundary can be reached.

## Finishing after cancellation {#after-cancellation}

The request's cancellation token is also cancelled when the client goes away
before it has the whole response.
Whichever way it is cancelled, the dispatch then has 5 seconds, by the clock, to
finish. This counts time spent waiting, which running time does not. A dispatch
that finishes in time, whether it returns a response or throws, leaves its
instance as it was, ready for the next request. A dispatch still running or
waiting when the 5 seconds are up is stopped, and its instance with it.

An instance handles several requests at once, so stopping it fails every
request it was handling, not only the one that ignored its token. While a
request is finishing after cancellation, Isoplane sends no new requests to its
instance, so only the requests already there are at risk. Handling cancellation
promptly protects them:

- Pass the token to every call you await, such as outbound HTTP calls, database
  commands and `Task.Delay`.
- Give a wait for an asynchronous lock a timeout or the token, for example
  `await gate.WaitAsync(TimeSpan.FromSeconds(1), cancellationToken)`. A wait
  with neither does not notice the cancellation, and lasts as long as the
  request holding the lock does.
- Let `OperationCanceledException` end the request. Catching it and starting
  more work uses up the 5 seconds.

## Work after the response {#after-the-response}

At the empty-stack boundary after response completion, remaining operations and
callbacks transfer to the instance's background allowance. The request releases
its concurrency slot; unused measured request running time is donated once to
the allowance. Background dispatches do not receive a new request allowance.

Each instance starts with 5 seconds of background running time, replenishes 5
seconds per 10 seconds of elapsed host time, and holds at most 30 seconds.
Donations also replenish it. Actual execution is charged, including any overrun;
each new background slice requires at least 5 seconds of credit. Below that
threshold, background execution waits for time refill or request donations.
Requests awaiting shared background work, such as a retained Redis connection,
also wait while that work is throttled. Library timeouts may be reported only
after background execution resumes.

Background slices use the engine's existing grace margin and hard execution
limit. A callback that fails to return can still cause the instance to be stopped.

Timers and I/O completions do not create credit. Replacement instances start
with the initial allowance. Background activity does not keep an instance alive:
an instance that holds retained work or connections is discarded five minutes
after the last active request finishes, unless another request arrives.
See [Background work](dotnet-runtime-differences.md#background-work).

HTTP requests have no overall wall-time deadline while the client remains
connected. Streaming responses keep their request slot until the body completes.
Their occupied execution time accumulates across every slice against the same
request budget; waiting for I/O does not consume that budget.

## Pending operations

Each request has its own pending-operation cap. Shared native handles and
connections have instance caps. The aggregate retained-future cap is the
native-handle limit plus eight requests' pending-operation limits; handing work
over or completing it does not free capacity until its result is delivered or
the operation is released. Filling this shared cap refuses a new operation
without discarding the instance. Exceeding a dispatch's own pending-operation
cap remains a trap.

Undelivered future payloads share a 256 KiB instance budget, including while the
instance is resident. Upload reads wait for result space before copying available
transport data into a result; large reads may return fewer bytes. Delivery or
cancellation frees that space. Other completions that exceed available space
return a buffer-limit error. This budget does not include transport buffers,
response queues or the guest heap.

## Timers

An instance can hold 16 host timers at once. .NET groups `Timer`, `Task.Delay`
and `CancellationTokenSource.CancelAfter` by their owning request, with a separate
group for retained background timers. Each group shares a host timer; individual
managed timers do not each consume a host timer. Request timers continue under
their request's allowance when background execution is throttled.
Creating a seventeenth host timer stops the instance.

## Outbound connections

The [local defaults](#local-defaults) include a concurrent outbound-connection
limit per instance.
Set `concurrentConnectionsPerInstance` in [local settings](../how-to/local-settings.md) to
test a lower limit. The host network ceiling still applies. A connection beyond
the effective limit fails with a socket error. A connection counts from when it
starts to open until the application closes or disposes of it, including while the instance is
idle between dispatches, so the connections a connection pool keeps open count
too.

Connections remain open until the application, its peer, or instance cleanup
closes them. An idle connection still counts toward the instance's limit.

An instance can also have six outbound connections being opened at once. A
further connection does not fail: it waits, in order, until one of the six is
established or fails, so it takes longer to open. Connections that are already
open do not delay new ones.

Other instances, including those of the same application, do not use up an
instance's connection allowance.

## Local defaults

These are preliminary limits for the initial SDK release. They may increase or
decrease in later releases.
The table is generated from the implementation's default limits.

<LocalLimits />

## Try smaller limits locally

To see how an application behaves under smaller limits, see
[Try the application under tighter limits](../how-to/local-settings.md#try-the-application-under-tighter-limits).
