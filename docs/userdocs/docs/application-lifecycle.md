---
title: The application lifecycle
---

Isoplane may reuse an application instance for subsequent requests or discard it
after a request finishes. Successive requests may reach different instances, so
in-memory state and open connections are not guaranteed to survive between
requests. Store any state that needs to persist in a database external to the instance.

## A request owns its work

The request lifetime bounds its work. Execution after the response is
unsupported: starting a task does not give that task a right to finish. Awaiting
request work and propagating cancellation keeps its completion tied to the
request that owns it.

Asynchronous network APIs allow an instance to wait without retaining a live
execution stack. Waiting synchronously for network activity is not supported.
Reading or setting a supported socket configuration value is a different
operation from waiting for network traffic.
[Runtime limitations](runtime-limitations.md) lists the calls this rules out and
what to use instead.

## Pools belong to instances

The platform chooses how many application instances run. A database connection
pool limit applies to one instance, so it does not cap your application's total
database connections. Use your database provider's external pooler when you need
to manage connections across instances.

Disposing connections and other request resources releases them when their work
is finished. Reuse is an optimization: a stale connection or a cancelled request
is possible even when the previous request completed successfully.

## Preparation and activation are different

Application initialization can happen before an instance starts serving requests.
In `Program.cs`, subscribe to [`Isoplane.Initialization.Initializing`](reference/sdk/Isoplane.Sdk/Isoplane/Initialization/Initializing.md)
before awaiting `RunAsync` to warm reusable state before the template is
captured, such as serialization metadata or a compiled EF model. An application
[without ASP.NET](handle-requests-without-aspnet.md) marks one method `[Initializer]` instead. Callbacks return
a `Task` and are awaited in registration order before capture. Asynchronous
network I/O follows the application's normal outbound policy. Await all I/O and
dispose its connections, streams, and timers before the callback finishes; live
host resources cannot be captured. Keep request-scoped services and background
work out of these callbacks. An exception fails template preparation.

Static state created during initialization can be copied into multiple instances.
A request identifier, token or application-managed random seed stored in that
initial state is consequently not guaranteed to be unique to each instance.

Set state that must differ between instances when an instance activates:
subscribe to `Isoplane.Activation.Activated`, or mark one method `[Activated]`.
Generate request-specific values while handling the request. If a dependency
maintains its own random state, check its activation and reseeding requirements;
copying application state does not make that state unique.

Before those callbacks run, the runtime reseeds every `Random` that was created
without a seed, including `Random.Shared`, and the generator behind `Activity`
trace and span identifiers. Cryptographic randomness needs no reseeding, because
every call draws from the host. An ASP.NET application's
`HttpContext.TraceIdentifier` is drawn for each request, so instances do not
number their requests alike. Three things are left to you:

- a `Random` constructed with a seed, even when that seed was itself random;
- a generator that is not a `System.Random`, such as one a library implements;
- a value already drawn during initialization, which is copied as data.

## Data protection keys

ASP.NET Core Data Protection, which antiforgery tokens and authentication cookies
rely on, creates its key when the host starts. That is during initialization, so
the key is part of the template: instances from the same template protect and
unprotect with the same key. Fresh initialization may create a different key;
reusing a template retains its captured key. Data protected with an earlier key
needs that key to remain readable.

For application changes, use [the onboarding guide](onboard-an-application.md)
and [runtime development settings](local-settings.md).
