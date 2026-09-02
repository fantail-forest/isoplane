# Observability

## Emit one completion event per top-level operation, guaranteed

Have every dispatch, deploy, hibernate, wake, and migration emit exactly one completion event with its identifiers, duration, and outcome, from a guard that runs on success, failure, and panic. Treat these events as the source of truth for load, error rates, latency, and resource usage.

**Not this:** Start and end log lines for every internal function, or only logging on error.

**Exception:** Nested operations that are independently meaningful (a pre-initialisation pass inside a deploy) may emit their own at debug level.

## Emit lifecycle transitions as events with before and after

When an instance changes tier (hot, parked, hibernated, tombstoned) or a version changes route state, emit a structured event with the previous state, the new state, the cause, and the identifiers. These are where investigations start.

**Not this:** Inferring a transition from a counter delta.

**Exception:** None.

## Carry context as span fields, not string prefixes

Put tenant, app, version, instance, and dispatch identifiers on the enclosing span as fields, so code inside the span emits events without repeating them.

**Not this:** Formatting `[app=quotes v3]` into every message.

**Exception:** None.

## Show the operator only what a test can assert

Produce every figure on a dashboard from an event or span that a test observes. A metric no test reads is a metric that can silently go to zero.

**Not this:** Dozens of counters of which several count faults of an instrument that no longer exists.

**Exception:** None.
