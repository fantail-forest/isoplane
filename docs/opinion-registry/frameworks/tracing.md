# tracing

## Open a span at each stage of a dispatch and record one completion per dispatch

Implement the [observability contract](../../../AGENTS.md#observability) with one tracing mechanism: nested stage spans explain where time went, and the wide completion event brings the final context together. Carry dispatch context across parks so a hot interval does not look like a complete request. See [completion ownership](../production/observability.md#make-completion-ownership-survive-handled-failures) for handled failures and delivery limits.

**Not this:** A bespoke timestamp-pool instrument with over a hundred labelled points, sampled one-in-N, whose only consumer is a benchmark and which runs in production anyway.

**Exception:** None.

## Export over OTLP; dashboards read spans and events

Send traces and events out of the process through an exporter, and have operator dashboards and alerting read the exported data. Export standalone metrics the same way where a counter or gauge fits better than a span. Plain log lines are welcome for what they are good at; nothing critical depends on scraping their text, and nothing critical rests on a counter that resets when the process restarts.

**Not this:** A text metrics endpoint as the primary interface and log capture as the debugging story.

**Exception:** A health endpoint for the load balancer.

## Never log guest memory or secrets

The [observability data restrictions](../../../AGENTS.md#observability) apply to span fields and wide events as well as log messages. Identifiers and sizes support correlation without capturing request bodies or delta contents.

**Not this:** Logging a request object wholesale.

**Exception:** None.

## Emit errors as events with the same fields as completions

Use the [terminal-error boundary](../../../AGENTS.md#error-handling) to attach established tenant, app, version, instance, operation, and limit context to the completion event. A standalone error event is useful when there is no operation completion to carry it; propagating layers do not need to repeat the same terminal record.

**Not this:** `"budget exceeded"`.

**Exception:** Messages returned to an untrusted client are generic; the detail goes to the event.
