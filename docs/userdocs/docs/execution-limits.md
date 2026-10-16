---
title: Execution limits
---

Isoplane limits what one instance and one
[dispatch](dotnet-runtime-differences.md#units-of-work) may use, so that no
application can take resources from the others running beside it. A dispatch is
a piece of work Isoplane gives an instance, usually one HTTP request to handle.
These limits are enforced by the platform. They are separate from the ways
[.NET itself behaves differently](dotnet-runtime-differences.md) in the sandbox.

## Memory

Each instance has a memory limit and cannot grow past it. The limit applies to
the instance as a whole, so whatever startup loaded and whatever an in-memory
cache holds counts alongside the request being handled.

To keep a large request or response out of memory, read and write it as a
stream: [`StreamingRequest`](reference/sdk/Isoplane.Sdk/Isoplane/StreamingRequest.md)
and [`Response.Stream`](reference/sdk/Isoplane.Sdk/Isoplane/Response/Stream.md)
without ASP.NET, or the request and response body streams in an ASP.NET
application.

## Running time

Each dispatch has an allowance of running time. Running time is the time the
dispatch spends executing. Time spent waiting for something it awaits, such as a
database or another service, does not count.

When the allowance runs out, the request's cancellation token is cancelled:
`HttpContext.RequestAborted`, or
[`RequestHead.Aborted`](reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
without ASP.NET. The dispatch then has a short further period in which to finish.
If it does not, Isoplane stops the instance, and `finally` blocks in the
dispatch's code do not run.

Cancellation reaches the application only when it awaits. Like the
[clock](dotnet-runtime-differences.md#time), the token stands still while code
runs: `IsCancellationRequested` and `ThrowIfCancellationRequested` give the
answer from when the application last resumed, so a loop that checks them
without awaiting never sees the token cancel, and is stopped when the further
period ends. To stop a long computation cleanly, split it into parts and, between
them, await something that waits, such as `await Task.Delay(1, cancellationToken)`.
`await Task.Yield()` is not enough: the code after it runs before the application
waits.

## Pending operations

Each dispatch can have only so many asynchronous operations waiting at once, such
as outbound calls and timers. A dispatch that goes over that number fails. When a
dispatch fans out over many items, start them in batches and await each batch.

## Timers

An instance can hold 16 timers at once. .NET keeps every `Timer`, `Task.Delay`
and `CancellationTokenSource.CancelAfter` of an application behind a single one,
so this limit is reached only by native code that creates timers of its own.
Creating a seventeenth stops the instance.

## Outbound connections

An instance can hold 32 outbound connections open at once. A connection beyond
that fails with a socket error. A connection counts from when it starts to open
until the application closes or disposes of it, including while the instance is
idle between dispatches, so the connections a connection pool keeps open count
too.

An instance can also have six outbound connections being opened at once. A
further connection does not fail: it waits, in order, until one of the six is
established or fails, so it takes longer to open. Connections that are already
open do not delay new ones.

These are the only limits on outbound connections. Other instances, including
other instances of the same application, do not use up an instance's
connections.

## Try smaller limits locally

A local session runs with the largest limits the platform offers. To see how an
application behaves under smaller ones, see
[Try the application under tighter limits](local-settings.md#try-the-application-under-tighter-limits).
