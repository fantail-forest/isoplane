---
title: Write for the application lifecycle
---

Your application may handle successive requests in different instances. Keep
state that must survive a request in an external service, such as your database.
In-memory state and open connections are not guaranteed to survive between
requests.

## Finish work before responding

Await work that belongs to the request before returning its response. Execution
after the response is unsupported: starting a task does not give that task a
right to finish. Pass request cancellation through to operations that accept it.

Use asynchronous network APIs, such as `HttpClient.SendAsync` and database
methods ending in `Async`. Waiting synchronously for network activity is not
supported. Reading or setting a supported socket configuration value is a
different operation from waiting for network traffic.

## Keep connection pools in perspective

The platform chooses how many application instances run. A database connection
pool limit applies to one instance, so it does not cap your application's total
database connections. Use your database provider's external pooler when you need
to manage connections across instances.

Dispose connections and other request resources when finished. Reusing a
connection is an optimization; handle stale connections and cancellation rather
than relying on an instance staying alive.

## Treat startup and activation separately

Application initialization can happen before an instance starts serving requests.
Static state created during initialization can be copied into multiple instances.
Do not prepare request identifiers, tokens, or application-managed random seeds
in static fields and assume that each instance receives a unique value.

Generate request-specific values while handling the request. If a dependency
maintains its own random state, check its activation and reseeding requirements;
copying application state does not make that state unique.
