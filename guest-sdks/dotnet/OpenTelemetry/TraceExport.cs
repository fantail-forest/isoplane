using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.Logging;
using OpenTelemetry;
using OpenTelemetry.Exporter;

namespace Isoplane.OpenTelemetry;

/// <summary>
/// The spans <see cref="IsoplaneOtlpExporterExtensions.AddIsoplaneOtlpExporter"/> holds and sends.
/// Take it as a dependency to send them before the request that holds them ends.
/// </summary>
public sealed class TraceExport
{
    private readonly ILogger logger;
    private readonly object gate = new();
    private readonly Queue<EncodedExport> encoded = new();
    private DeferredExportProcessor? processor;
    private Func<HttpClient> createSender = CreateSender;
    private HttpClient? sender;
    private int timeoutMilliseconds;
    private int maxEncodedExports = 1;
    private bool sending;
    private Task<bool> sent = Task.FromResult(true);

    internal TraceExport(ILogger<TraceExport> logger) => this.logger = logger;

    /// <summary>
    /// Sends the spans that have ended so far, including those of requests still running, and
    /// waits until every export started before it has been answered.
    /// </summary>
    /// <param name="cancellationToken">Stops the wait; exports already started still finish.</param>
    /// <returns>Whether the endpoint accepted every export it waited for.</returns>
    /// <remarks>
    /// Spans are otherwise held until their request's span ends. Flush first when a request
    /// produces many spans, or when it should not end before its telemetry has been sent.
    /// </remarks>
    /// <exception cref="OperationCanceledException"><paramref name="cancellationToken"/> was cancelled.</exception>
    public Task<bool> FlushAsync(CancellationToken cancellationToken = default)
    {
        processor?.EncodeHeld();
        return SendEncoded().WaitAsync(cancellationToken);
    }

    internal ILogger Logger => logger;

    internal DeferredExportProcessor CreateProcessor(IConfiguration? configuration, Action<OtlpExporterOptions>? configure)
    {
        if (processor is not null)
            throw new InvalidOperationException("AddIsoplaneOtlpExporter can be called once for an application.");
        ExposeOtlpSettings(configuration);
        var options = new OtlpExporterOptions();
        var defaultFactory = options.HttpClientFactory;
        configure?.Invoke(options);
        createSender = ReferenceEquals(options.HttpClientFactory, defaultFactory) ? CreateSender : options.HttpClientFactory;
        timeoutMilliseconds = options.TimeoutMilliseconds;
        bool grpc = options.Protocol == OtlpExportProtocol.Grpc;
        options.HttpClientFactory = () => new HttpClient(new EncodedExportCapture(this, grpc));
        var batches = options.BatchExportProcessorOptions;
        maxEncodedExports = Math.Max(1, batches.MaxQueueSize / batches.MaxExportBatchSize);
        processor = new DeferredExportProcessor(new OtlpTraceExporter(options), this, batches.MaxExportBatchSize);
        return processor;
    }

    // OtlpExporterOptions reads the OTLP settings from environment variables, and an instance
    // has none: the application's own OTEL_ settings are placed where the exporter reads them.
    private static void ExposeOtlpSettings(IConfiguration? configuration)
    {
        if (configuration is null) return;
        foreach (var (key, value) in configuration.AsEnumerable())
        {
            if (value is not null && key.StartsWith("OTEL_", StringComparison.Ordinal)
                && Environment.GetEnvironmentVariable(key) is null)
                Environment.SetEnvironmentVariable(key, value);
        }
    }

    // A connection kept between requests counts against the instance's outbound connections,
    // so each one closes once its export is answered.
    private static HttpClient CreateSender() =>
        new(new SocketsHttpHandler { AllowAutoRedirect = false, PooledConnectionLifetime = TimeSpan.Zero })
        {
            Timeout = Timeout.InfiniteTimeSpan,
        };

    internal void Enqueue(EncodedExport export)
    {
        lock (gate)
        {
            if (encoded.Count < maxEncodedExports)
            {
                encoded.Enqueue(export);
                return;
            }
        }
        logger.LogWarning(
            "Dropped an OTLP export to {Endpoint}: {Waiting} exports are already waiting to be sent.",
            export.Uri, maxEncodedExports);
    }

    internal Task<bool> SendEncoded()
    {
        TaskCompletionSource<bool> done;
        lock (gate)
        {
            if (sending || encoded.Count == 0) return sent;
            sending = true;
            done = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            sent = done.Task;
        }
        _ = SendAllAsync(done);
        return done.Task;
    }

    private async Task SendAllAsync(TaskCompletionSource<bool> done)
    {
        using var suppressed = SuppressInstrumentationScope.Begin();
        bool delivered = true;
        try
        {
            while (true)
            {
                EncodedExport? export;
                lock (gate)
                {
                    if (!encoded.TryDequeue(out export))
                    {
                        sending = false;
                        break;
                    }
                }
                delivered &= await DeliverAsync(export).ConfigureAwait(false);
            }
            done.SetResult(delivered);
        }
        catch (Exception error)
        {
            lock (gate) sending = false;
            logger.LogError(error, "Sending OTLP exports stopped.");
            done.SetException(error);
        }
    }

    private async Task<bool> DeliverAsync(EncodedExport export)
    {
        using var timeout = new CancellationTokenSource(timeoutMilliseconds);
        try
        {
            using var request = export.ToRequest();
            using var response = await (sender ??= createSender())
                .SendAsync(request, HttpCompletionOption.ResponseContentRead, timeout.Token).ConfigureAwait(false);
            if (export.Accepted(response, out var refusal)) return true;
            logger.LogWarning("The OTLP endpoint {Endpoint} refused an export: {Refusal}.", export.Uri, refusal);
        }
        catch (HttpRequestException error)
        {
            logger.LogWarning(error, "An OTLP export to {Endpoint} could not be sent.", export.Uri);
        }
        catch (OperationCanceledException) when (timeout.IsCancellationRequested)
        {
            logger.LogWarning("An OTLP export to {Endpoint} was not answered within {TimeoutMilliseconds} ms.",
                export.Uri, timeoutMilliseconds);
        }
        return false;
    }
}
