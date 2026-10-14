---
title: Execution limits
---

Isoplane limits what one instance and one request may use, so that no
application can take resources from the others running beside it. These limits
are enforced by the platform. They are separate from the ways
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

Each request has an allowance of running time. Running time is the time the
request spends executing. Time spent waiting for something it awaits, such as a
database or another service, does not count.

When the allowance runs out, the request's cancellation token is cancelled:
`HttpContext.RequestAborted`, or
[`RequestHead.Aborted`](reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md)
without ASP.NET. The request then has a short further period in which to finish.
If it does not, Isoplane stops the instance, and `finally` blocks in the request
do not run.

Cancellation reaches the application only while it is waiting, so a request that
computes without awaiting never sees the token cancel and is stopped when the
further period ends.

## Pending operations

Each request can have only so many asynchronous operations waiting at once, such
as outbound calls and timers. A request that goes over that number fails. When a
request fans out over many items, start them in batches and await each batch.

## Outbound connections

An instance can hold 32 outbound connections open at once. A connection beyond
that fails with a socket error. A connection counts from when it starts to open
until the application closes or disposes of it, including while the instance is
idle between requests, so the connections a connection pool keeps open count
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
