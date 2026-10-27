---
title: "The application lifecycle: who does what"
---

An application on Isoplane goes through five stages, and Isoplane decides when
each one happens. For each stage, this page says what Isoplane takes care of and
what is left to your application.

| Stage | When | Isoplane | Your application |
| --- | --- | --- | --- |
| [Initialization](#initialization) | When creating a template, before requests | Runs `Program.cs` and saves the application's memory as a template | Prepares shared state, and closes every connection it opened |
| [Activation](#activation) | Each time an instance starts | Copies the template and reseeds random generators | Replaces copied state that has to be unique to each instance, such as an identifier |
| [Handling a request](#handling-a-request) | Each request | Delivers the request, suspends the instance while it waits, enforces limits | Awaits its work, and forwards the request's cancellation token to the asynchronous calls it makes |
| [Between requests](#between-requests) | Whenever an instance has no request | Keeps the instance and its pending timers for later, or discards it | Expects nothing to run on time and nothing to survive |
| [A new build or changed settings](#a-new-build-or-changed-settings) | When you change the application or its settings | Prepares a new template when needed; matching captured state may be reused | Keeps what must outlive a build somewhere else |

An *instance* is one running copy of the application. Isoplane runs as many as
the traffic needs, up to the application’s instance limit. The *template* is the
memory every instance starts from.

## Initialization

Isoplane runs initialization while creating a template, ahead of requests. In an ASP.NET
application it runs `Program.cs` up to `RunAsync`, then raises
[`Initialization.Initializing`](../reference/sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing.md).
When the callbacks have finished it saves the application's memory as the
template.

```csharp
var app = builder.Build();
app.MapGet("/todos", (TodoStore store) => store.All());

// Runs during template preparation; retries can run it again.
Initialization.Initializing += async () =>
{
    await app.WarmupAsync("/todos");
};

await app.RunAsync();
```

**Isoplane**

- Runs `Program.cs` during preparation, with the application's settings and the network
  available to it.
- Awaits the `Initializing` callbacks one at a time, in the order they
  subscribed.
- Saves everything left in memory, and starts every instance from it.
- Refuses to start the application if initialization throws, or if a
  connection or stream is still open when the callbacks have finished.
- Saves timers and `BackgroundService` loops still running when the callbacks
  have finished with the template, and carries them on in each instance
  [between requests](#between-requests). A timer counts from when that
  instance starts.

**Your application**

- Does its expensive one-time work here, so that no request pays for it: load
  reference data, build lookup tables, and serve itself a request with
  [`WarmupAsync`](../reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/IsoplaneHosting/WarmupAsync.md)
  so that routes, serializers and database models are ready. Given a path it
  sends a `GET`; given an `HttpRequestMessage` it sends any method, with headers
  and a body, for a route such as a search that takes a `POST`. Preparation attempts
  can serve these requests again, so send requests safe to repeat. Reusing a
  prepared template sends none.
- Awaits all I/O, and closes every connection and stream before the callbacks
  return. An open connection cannot be saved in the template, so
  Isoplane refuses to start an application that leaves one. A database driver's
  connection pool keeps connections open after use, so turn pooling off, as
  [Connect to PostgreSQL](../how-to/connect-postgresql.md) does.
- Creates nothing that should be unique to an instance. Whatever initialization leaves
  in memory, including a random value or an identifier in a static field, is the
  same in every instance.
- May leave timers running. A timer with 30 s left when initialization ends
  becomes eligible 30 s after an instance starts, as
  [Background work](dotnet-runtime-differences.md#background-work)
  describes.
- Starts no background work.

**What can go wrong**

In each case the application does not start, and the terminal shows the reason.

| Condition | What you see |
| --- | --- |
| `Program.cs` or an `Initializing` callback throws | The exception that was thrown |
| A connection, stream or DNS result is still open when the callbacks finish | A message that names the kind, such as `initialization retains a TCP socket; dispose it before capture` |
| Code waits synchronously or starts a thread | `PlatformNotSupportedException`, unless the code catches it |
| Initialization runs longer than Isoplane allows | Initialization is stopped |
| The entry point calls `Run`, or does not await `RunAsync` | A build error, [`ISOPLANE1020`](../reference/error-codes.md#isoplane1020) or [`ISOPLANE1021`](../reference/error-codes.md#isoplane1021), before anything runs |

A setting read during initialization is saved in the template with everything else, and
every instance has the same settings.

An application [without ASP.NET](../how-to/handle-requests-without-aspnet.md#lifecycle-methods)
has no `Program.cs`, and marks one method `[Initializer]` instead.

## Activation

An instance starts as a copy of the template. Before it handles its first
request, Isoplane raises
[`Activation.Activated`](../reference/sdk/Isoplane.Sdk/Isoplane/Activation/Activated.md).

```csharp
var instance = Guid.Empty;

// Runs in each instance, before its first request.
Activation.Activated += () =>
{
    instance = Guid.NewGuid();
    return Task.CompletedTask;
};
```

**Isoplane**

- Gives the instance its own copy of the template's memory.
- Reseeds `Random.Shared`, every `Random` that was created without a seed, the
  generator behind `Activity` trace and span identifiers, and the one behind
  SQLite's `random()` and `randomblob()`. Cryptographic randomness needs no
  reseeding: every call draws fresh bytes.
- Awaits the `Activated` callbacks one at a time, in the order they subscribed.
  An exception from one fails the request the instance was started for.

**Your application**

- Subscribes during initialization. Subscriptions are saved in the template, so every
  instance runs them.
- Replaces any value copied from the template that has to be unique to the
  instance, such as an identifier or a token.
- Reseeds what Isoplane cannot: a `Random` constructed with a seed, even when
  that seed was itself random, and a generator that is not a `System.Random`,
  such as one inside a library.

**What can go wrong**

| Condition | What happens |
| --- | --- |
| An `Activated` callback throws | The request the instance was started for fails, and the instance is not used again. The next request starts another instance, which runs the callbacks again. |
| Initialization left more in memory than an instance is allowed | No instance can start. Reduce what initialization keeps. |

An application without ASP.NET marks one method `[Activated]` instead.

## Handling a request

Isoplane hands a request to one instance. While the request waits on an
asynchronous operation, other requests and eligible background work can run on
the same instance. When nothing is ready to run, the instance parks without
occupying a worker thread.

**Isoplane**

- Delivers the request and returns the response.
- Gives each ASP.NET request its own `HttpContext.TraceIdentifier`.
- Cancels the request's token, `HttpContext.RequestAborted` or
  [`RequestHead.Aborted`](../reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md),
  when the client goes away before it has the whole response, or the request's
  [dispatch](dotnet-runtime-differences.md#dispatches-and-slices) has used its running
  time.
- Enforces the [execution limits](execution-limits.md). It stops an instance
  whose dispatch keeps running after its running time is used up, or has not
  [finished 5 seconds after its token was cancelled](execution-limits.md#after-cancellation),
  and the other requests on that instance fail with it.
- Ends the request at the empty-stack boundary after response completion,
  releasing its concurrency slot. Remaining operations, timers and their
  accounting transfer to the instance's background allowance; see
  [Background work](dotnet-runtime-differences.md#background-work).

**Your application**

- Awaits everything the request needs before it responds. Work started and not
  awaited may never finish.
- Uses asynchronous APIs throughout; see
  [how .NET differs on Isoplane](dotnet-runtime-differences.md#blocking-waits).
- Forwards the cancellation token to the asynchronous calls it makes.
- Disposes what it opens. Do not leave cleanup to a finalizer.
- Can use `IHttpClientFactory` with normal connection pooling and handler
  lifetimes; see
  [HTTP clients and connection reuse](dotnet-runtime-differences.md#http-clients).
- Saves anything that must last to a database or another external service
  before it responds.

**What can go wrong**

| Condition | What happens |
| --- | --- |
| An ordinary handler exception is not handled | That request receives status 500 if its response has not started; otherwise its response is aborted. The instance and other requests remain available. |
| Code waits synchronously or starts a thread | The call throws `PlatformNotSupportedException` |
| The client goes away before it has the whole response | The request's token is cancelled |
| The dispatch uses up its running time | The token is cancelled. If the dispatch has not finished shortly afterwards, Isoplane stops the instance and the request fails. |
| The dispatch exceeds its own pending-operation cap | The instance traps; all its requests fail |
| Retained work fills the instance-wide future cap | Starting another operation fails; consuming or releasing existing work frees capacity |
| The response is complete while work the request started is still pending | The request ends at the empty-stack boundary and the work transfers to the instance's background allowance. A pending request-body read fails. See [Background work](dotnet-runtime-differences.md#background-work). |

## Between requests

An instance may run background callbacks and retain connections between requests.
Isoplane may hibernate or discard it. Successive requests can reach different
instances.

**Isoplane**

- Decides how many instances run and which one gets a request, within the
  application's instance limit, including instances finishing cancelled requests.
- Discards instances without a shutdown callback or guest cleanup.
- Runs ready background work under an instance allowance. A wake does not
  create a new request allowance. Exhausted allowance delays that work.
- Retains connections until the application, peer or instance cleanup closes
  them. Retained connections still count against instance limits.
- Stops an idle instance that has background work or open connections five
  minutes after the last active request finishes, unless another request
  arrives.
  Only request arrivals refresh demand recency. The idle stop also releases
  hibernated instances without waking them to run background work.

**Your application**

- Relies on no loop or queue consumer to run in the background, and on no
  timer to run on time. A `BackgroundService` carries on between requests only
  while its instance is resident.
- Treats memory as a cache. Static fields and `IMemoryCache` belong to one
  instance and vanish with it.
- Expects a connection kept from an earlier request to be stale, and opens
  another when it is.
- Sizes any database pool for one instance. An
  [external pooler](dotnet-runtime-differences.md#outgoing-connections)
  can cap database connections across the application.

**What can go wrong**

| Condition | What happens |
| --- | --- |
| Isoplane discards the instance | Nothing is reported. Its memory is gone, and the next request reaches an instance that starts from the template. |
| A connection kept from an earlier request has gone stale | Its next use fails as a dropped connection does anywhere. Open a new one. |
| Isoplane discards an instance whose `BackgroundService` is still running | The service stops where it was. Its stopping token is not cancelled and `StopAsync` does not run. |
| A timer comes due while the instance is idle | It becomes eligible for a [background slice](dotnet-runtime-differences.md#background-work), subject to allowance and instance residency. It never runs if the instance is discarded first. |

## A new build or changed settings

When preparation inputs change, Isoplane prepares a new template. Local
development may reuse matching captured state after a restart or rebuild,
without rerunning initialization callbacks.

**Isoplane**

- Runs initialization when a new template is needed; reusing captured state
  runs no callbacks.
- Starts instances from the selected template.

**Your application**

- Keeps what must outlive a build outside the instance.
- Does not depend on data protected by ASP.NET Core Data Protection outliving a
  build, unless it [keeps its keys across builds](../how-to/keep-data-protection-keys.md).
  By default the key is created during initialization, so it is part of the
  template: every instance of the same template shares it, which keeps antiforgery
  tokens and authentication cookies working across instances. Fresh initialization
  may create a different key; reusing a template retains its key.

**What can go wrong**

| Condition | What happens |
| --- | --- |
| The rebuild fails under `isoplane dev` | The terminal says why, and the last working application stays available |
| Initialization fails for the new build | The same as under [Initialization](#initialization) |
| A cookie or token was protected with a key absent from the new template | It cannot be unprotected unless that key is retained |
