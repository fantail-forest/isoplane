using System.Diagnostics;
using Microsoft.Extensions.Logging;
using OpenTelemetry;

namespace Isoplane.OpenTelemetry;

/// <summary>
/// Holds finished spans until a span with no parent in the application ends, then encodes them
/// with the exporter, whose requests <see cref="EncodedExportCapture"/> keeps for
/// <see cref="TraceExport"/> to send.
/// </summary>
internal sealed class DeferredExportProcessor(BaseExporter<Activity> exporter, TraceExport export, int maxBatchSpans)
    : BaseExportProcessor<Activity>(exporter)
{
    private readonly List<Activity> held = [];
    private readonly object encoding = new();

    protected override void OnExport(Activity data)
    {
        bool send;
        lock (held)
        {
            held.Add(data);
            send = data.Parent is null || held.Count >= maxBatchSpans;
        }
        if (!send) return;
        EncodeHeld();
        _ = export.SendEncoded();
    }

    internal void EncodeHeld()
    {
        Activity[] spans;
        lock (held)
        {
            if (held.Count == 0) return;
            spans = [.. held];
            held.Clear();
        }
        using var suppressed = SuppressInstrumentationScope.Begin();
        // The exporter is not built to be called concurrently.
        lock (encoding)
        {
            for (int start = 0; start < spans.Length; start += maxBatchSpans)
            {
                var batch = spans[start..Math.Min(spans.Length, start + maxBatchSpans)];
                if (exporter.Export(new Batch<Activity>(batch, batch.Length)) != ExportResult.Success)
                    export.Logger.LogWarning("The OTLP exporter could not encode {Spans} spans.", batch.Length);
            }
        }
    }

    protected override bool OnForceFlush(int timeoutMilliseconds)
    {
        EncodeHeld();
        return export.SendEncoded().IsCompleted;
    }

    protected override bool OnShutdown(int timeoutMilliseconds)
    {
        EncodeHeld();
        _ = export.SendEncoded();
        return base.OnShutdown(timeoutMilliseconds);
    }
}
