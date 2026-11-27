### Isoplane.OpenTelemetry

Exports OpenTelemetry traces to an OTLP endpoint. `AddIsoplaneOtlpExporter`
replaces `AddOtlpExporter` and sends each request's spans after its response
instead of blocking; `TraceExport.FlushAsync` sends the spans that have ended
so far.

- [Isoplane.OpenTelemetry API](https://isoplane.dev/docs/reference/sdk/Isoplane.OpenTelemetry.md)
