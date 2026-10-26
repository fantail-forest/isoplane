# Isoplane.OpenTelemetry

Send the OpenTelemetry traces of an Isoplane application to an OTLP endpoint,
such as the OpenTelemetry Collector.

OpenTelemetry's own `AddOtlpExporter` delivers nothing in Isoplane's sandbox:
it sends each export synchronously, and a sandboxed application cannot block.
`AddIsoplaneOtlpExporter` uses the same exporter and the same settings, and
sends each request's spans after its response.

```bash
dotnet add package Isoplane.OpenTelemetry
```

In `Program.cs`:

```csharp
using Isoplane.OpenTelemetry;
using OpenTelemetry.Trace;

builder.Services.AddOpenTelemetry()
    .WithTracing(tracing => tracing
        .AddAspNetCoreInstrumentation()
        .AddIsoplaneOtlpExporter());
```

It reads the `OTEL_EXPORTER_OTLP_*` settings from the application's
configuration, as `AddOtlpExporter` does. The default is gRPC to
`http://localhost:4317`; set `OTEL_EXPORTER_OTLP_PROTOCOL` to `http/protobuf`
for an endpoint that takes OTLP over HTTP.

Spans are held in the instance's memory until their request's own span ends,
and sent while the client already has the response. Take `TraceExport` as a
dependency and call `FlushAsync` to send the spans that have ended sooner.
Spans an instance still holds when it is stopped are lost, and a refused export
is logged and not retried. Metrics and logs are not covered.

It is a package of its own so that an application that does not use
OpenTelemetry never restores it. Keep this package at the same version as
`Isoplane.AspNetCore`.

Learn more at [isoplane.dev](https://isoplane.dev/docs/concepts/library-compatibility#opentelemetry).
