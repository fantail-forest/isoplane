---
title: The application lifecycle
---

An application instance is not a permanently running server process. Successive
requests may reach different instances, and in-memory state and open connections
are not guaranteed to survive between requests. An external database provides
persistence independently of which instance handles the next request.

## A request owns its work

The request lifetime bounds its work. Execution after the response is
unsupported: starting a task does not give that task a right to finish. Awaiting
request work and propagating cancellation keeps its completion tied to the
request that owns it.

Asynchronous network APIs allow an instance to wait without retaining a live
execution stack. Waiting synchronously for network activity is not supported.
Reading or setting a supported socket configuration value is a different
operation from waiting for network traffic.

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
Static state created during initialization can be copied into multiple instances.
A request identifier, token or application-managed random seed stored in that
initial state is consequently not guaranteed to be unique to each instance.

Generate request-specific values while handling the request. If a dependency
maintains its own random state, check its activation and reseeding requirements;
copying application state does not make that state unique.

For application changes, use [the onboarding guide](onboard-an-application.md)
and [runtime development settings](local-settings.md).
