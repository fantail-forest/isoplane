# Observability

## Prefer OpenTelemetry for service observability

Use maintained OpenTelemetry integrations and OTLP export wherever they fit logs,
metrics and traces. Preserve structured completion records and propagate context
across service boundaries. Qualify adoption incrementally without replacing
working instrumentation solely to standardize its transport.

**Not this:** Separate proprietary telemetry plumbing for each service or treating
sampled diagnostic telemetry as complete usage data.

**Exception:** Native profiling tools and existing metric endpoints remain useful
where they provide better evidence or an integration has not yet been qualified.

## Prefer wide events at operation boundaries

A wide event brings the identifiers, operation, outcome, duration, resource use, and relevant failure context together in one structured completion, so an operator can investigate an operation without reconstructing it from scattered log lines. Apply the [dispatch completion contract and its limits](../../../AGENTS.md#observability) to dispatches; use the same approach for deploy, hibernate, wake, and migration operations, with the fields meaningful to each. Enrich context through the operation and finalize it at the owning boundary, using nested spans for stage detail rather than separate completion events for each hot interval.

**Not this:** Start and end log lines for every internal function, or only logging on error.

**Exception:** Independently meaningful nested operations may have their own correlated completion events. Lifecycle transitions and ongoing health signals need events or metrics before an operation finishes.

## Make completion ownership survive handled failures

The execution boundary that catches a host panic owns the dispatch outcome; a completion guard or equivalent ownership mechanism prevents early returns and caught unwinding from dropping or duplicating the record. Exercise success, failure, cancellation, and caught-panic paths through that boundary and observe the completion event. Event emission and durable export are separate concerns: a guard cannot promise delivery after an abort, forced termination, crash, or exporter failure.

**Not this:** Calling a `Drop` guard an exactly-once delivery guarantee, or treating missing telemetry as evidence of zero usage.

**Exception:** Abrupt process loss is outside the handled-outcome guarantee; represent incomplete evidence as incomplete.

## Emit lifecycle transitions as events with before and after

When an instance changes tier (hot, parked, hibernated, tombstoned) or a version changes route state, emit a structured event with the previous state, the new state, the cause, and the identifiers. These are where investigations start.

**Not this:** Inferring a transition from a counter delta.

**Exception:** None.

## Carry context as span fields, not string prefixes

Put tenant, app, version, instance, and dispatch identifiers on the enclosing span as fields, so code inside the span emits events without repeating them.

**Not this:** Formatting `[app=quotes v3]` into every message.

**Exception:** None.

## Validate telemetry according to the decisions it drives

Keep dashboard figures traceable to real measurements with clear meanings. Test canonical completion records, resource usage and telemetry that drives critical alerts or control decisions through their owning boundaries. Reuse behavior tests and shared schema or export checks where they provide coverage. Diagnostic counters and dashboard presentations do not each require a dedicated test; verify their wiring according to the risk of an incorrect or missing value.

**Not this:** Counters for an instrument that no longer exists, or a separate expensive end-to-end test for every label and display value.

**Exception:** None.

## Preserve useful partial traces

Allow completed spans to leave the process while their root or sibling spans are still running. A hung or interrupted operation can leave useful diagnostic evidence even when its trace never completes. Bound buffering and export queues, expose dropped spans and known truncation, and do not present missing spans as proof that a stage never ran. Keep canonical completion records independent of diagnostic sampling and export policy.

**Not this:** Withholding every completed stage of a hung request, then deliberately discarding those stages at shutdown because the root never ended.

**Exception:** Bounded buffering for whole-trace sampling is appropriate when its diagnostic benefit justifies delayed visibility and potential loss. It does not establish atomic delivery or completeness across processes.
