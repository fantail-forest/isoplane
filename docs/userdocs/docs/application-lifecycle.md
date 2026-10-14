---
title: "The application lifecycle: who does what"
---

An application on Isoplane goes through five stages, and Isoplane decides when
each one happens. For each stage, this page says what Isoplane takes care of and
what is left to your application.

| Stage | When | Isoplane | Your application |
| --- | --- | --- | --- |
| [Initialization](#initialization) | When creating a template, before requests | Runs `Program.cs` and saves the application's memory as a template | Prepares shared state, and closes every connection and timer it opened |
| [Activation](#activation) | Each time an instance starts | Copies the template and reseeds random generators | Replaces copied state that has to be unique to each instance, such as an identifier |
| [Handling a request](#handling-a-request) | Each request | Delivers the request, suspends the instance while it waits, enforces limits | Awaits its work, and forwards the request's cancellation token to the asynchronous calls it makes |
| [Between requests](#between-requests) | Whenever an instance has no request | Keeps the instance for later, or discards it | Expects nothing to run and nothing to survive |
| [A new build or changed settings](#a-new-build-or-changed-settings) | When you change the application or its settings | Prepares a new template when needed; matching captured state may be reused | Keeps what must outlive a build somewhere else |

An *instance* is one running copy of the application. Isoplane runs as many as
the traffic needs. The *template* is the memory every instance starts from.

## Initialization

Isoplane runs initialization while creating a template, ahead of requests. In an ASP.NET
application it runs `Program.cs` up to `RunAsync`, then raises
[`Initialization.Initializing`](reference/sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing.md).
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
- Refuses to start the application if initialization throws, if a
  `BackgroundService` is still running once the host has started, or if a
  connection, stream or timer is still open when the callbacks have finished.

**Your application**

- Does its expensive one-time work here, so that no request pays for it: load
  reference data, build lookup tables, and serve itself a request with
  [`WarmupAsync`](reference/sdk/Isoplane.AspNetCore/Isoplane.AspNetCore/IsoplaneHosting/WarmupAsync.md)
  so that routes, serializers and database models are ready. Given a path it
  sends a `GET`; given an `HttpRequestMessage` it sends any method, with headers
  and a body, for a route such as a search that takes a `POST`. Preparation attempts
  can serve these requests again, so send requests safe to repeat. Reusing a
  prepared template sends none.
- Awaits all I/O, and closes every connection, stream and timer before the
  callbacks return. An open connection cannot be saved in the template, so
  Isoplane refuses to start an application that leaves one. A database driver's
  connection pool keeps connections open after use, so initialize with pooling
  off, as the [PostgreSQL sample](connect-postgresql.md) does.
- Creates nothing that should be unique to an instance. Whatever initialization leaves
  in memory, including a random value or an identifier in a static field, is the
  same in every instance.
- Starts no background work.

**What can go wrong**

In each case the application does not start, and the terminal shows the reason.

| Condition | What you see |
| --- | --- |
| `Program.cs` or an `Initializing` callback throws | The exception that was thrown |
| A `BackgroundService` is still running once the host has started | An error that names the service |
| A connection, stream, DNS result or timer is still open when the callbacks finish | A message that names the kind, such as `initialization retains a TCP socket; dispose it before capture` |
| Code waits synchronously or starts a thread | `PlatformNotSupportedException`, unless the code catches it |
| Initialization runs longer than Isoplane allows | Initialization is stopped |
| The entry point calls `Run`, or does not await `RunAsync` | A build error, [`ISOPLANE1020`](reference/error-codes.md#isoplane1020) or [`ISOPLANE1021`](reference/error-codes.md#isoplane1021), before anything runs |

A setting read during initialization is saved in the template with everything else, and
every instance has the same settings.

An application [without ASP.NET](handle-requests-without-aspnet.md#lifecycle-methods)
has no `Program.cs`, and marks one method `[Initializer]` instead.

## Activation

An instance starts as a copy of the template. Before it handles its first
request, Isoplane raises
[`Activation.Activated`](reference/sdk/Isoplane.Sdk/Isoplane/Activation/Activated.md).

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
- Reseeds `Random.Shared`, every `Random` that was created without a seed, and
  the generator behind `Activity` trace and span identifiers. Cryptographic
  randomness needs no reseeding: every call draws fresh bytes.
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
asynchronous operation, Isoplane suspends the instance and resumes it when the
operation completes.

**Isoplane**

- Delivers the request and returns the response.
- Gives each ASP.NET request its own `HttpContext.TraceIdentifier`.
- Cancels the request's token, `HttpContext.RequestAborted` or
  [`RequestHead.Aborted`](reference/sdk/Isoplane.Sdk/Isoplane/RequestHead/Aborted.md),
  when the client goes away or the request has used its running time.
- Enforces the [execution limits](execution-limits.md), and stops an instance
  whose request keeps running after its running time is used up.
- Cancels any timer or outbound call still pending when the response is
  complete, and then discards the instance, because the code waiting on that
  work could never resume.

**Your application**

- Awaits everything the request needs before it responds. Work started and not
  awaited may never finish.
- Uses asynchronous APIs throughout; see
  [how .NET differs on Isoplane](dotnet-runtime-differences.md#waiting-is-asynchronous).
- Forwards the cancellation token to the asynchronous calls it makes.
- Disposes what it opens. Do not leave cleanup to a finalizer.
- Takes its `HttpClient` from `IHttpClientFactory`, whose clients leave nothing
  pending, rather than keeping one it constructed itself; see
  [Network](dotnet-runtime-differences.md#network).
- Saves anything that must last to a database or another external service
  before it responds.

**What can go wrong**

| Condition | What happens |
| --- | --- |
| The application throws an exception it does not handle | That request fails. Without ASP.NET, the client receives status 502. |
| Code waits synchronously or starts a thread | The call throws `PlatformNotSupportedException` |
| The client goes away | The request's token is cancelled |
| The request uses up its running time | The token is cancelled. If the request has not finished shortly afterwards, Isoplane stops the instance and the request fails. |
| The request has too many asynchronous operations pending at once | The request fails |
| The response is complete while work is still pending | That work is cancelled and never finishes, and the instance is discarded: the next request starts from the template and pays for activation. No error is reported. |

## Between requests

An instance with no request does not run at all. Isoplane may keep it for the
next request, or discard it. Successive requests can reach different instances.

**Isoplane**

- Decides how many instances run and which one gets a request.
- Discards an instance whenever it chooses, without notice. No shutdown event is
  raised and no cleanup code runs.

**Your application**

- Relies on no timer, loop or queue consumer to run in the background.
- Treats memory as a cache. Static fields and `IMemoryCache` belong to one
  instance and vanish with it.
- Expects a connection kept from an earlier request to be stale, and opens
  another when it is.
- Pools database connections
  [outside the application](dotnet-runtime-differences.md#pool-database-connections-outside-the-application),
  because a pool inside it belongs to one instance.

**What can go wrong**

| Condition | What happens |
| --- | --- |
| Isoplane discards the instance | Nothing is reported. Its memory is gone, and the next request reaches an instance that starts from the template. |
| A connection kept from an earlier request has gone stale | Its next use fails as a dropped connection does anywhere. Open a new one. |

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
  build, unless it [keeps its keys across builds](dotnet-runtime-differences.md#keep-data-protection-keys-across-builds).
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
