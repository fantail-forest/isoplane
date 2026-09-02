# tracing

## Open a span at each stage of a dispatch and record one completion per dispatch

Make each stage a dispatch passes through a span, nested under the dispatch's own, so where its time went can be read per request. End every dispatch with one completion record carrying enough to attribute it, emitted from a guard so it cannot be skipped by an early return or a panic. Boundaries nest, and so do spans: a step inside a handler gets its own span under the handler's.

**Not this:** A bespoke timestamp-pool instrument with over a hundred labelled points, sampled one-in-N, whose only consumer is a benchmark and which runs in production anyway.

**Exception:** None.

## Export over OTLP; dashboards read spans and events

Send traces and events out of the process through an exporter, and have operator dashboards and alerting read the exported data. Export standalone metrics the same way where a counter or gauge fits better than a span. Plain log lines are welcome for what they are good at; nothing critical depends on scraping their text, and nothing critical rests on a counter that resets when the process restarts.

**Not this:** A text metrics endpoint as the primary interface and log capture as the debugging story.

**Exception:** A health endpoint for the load balancer.

## Never log guest memory or secrets

Put identifiers and sizes in events, never bytes from a request body, a delta, a key, or a token.

**Not this:** Logging a request object wholesale.

**Exception:** None.

## Emit errors as events with the same fields as completions

Make an error event name the tenant, app, version, instance, operation, and limit involved. Reading it should not require reproducing the failure.

**Not this:** `"budget exceeded"`.

**Exception:** Messages returned to an untrusted client are generic; the detail goes to the event.
