using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.DependencyInjection.Extensions;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Logging.Abstractions;
using OpenTelemetry.Exporter;
using OpenTelemetry.Trace;

namespace Isoplane.OpenTelemetry;

/// <summary>Sends an application's traces to an OTLP endpoint from Isoplane's sandbox.</summary>
public static class IsoplaneOtlpExporterExtensions
{
    /// <summary>
    /// Exports finished spans with OpenTelemetry's OTLP exporter, sending each request's spans
    /// after its response instead of blocking while they are sent, which the sandbox cannot do.
    /// Use it in place of <c>AddOtlpExporter</c>.
    /// </summary>
    /// <param name="builder">The tracing builder, as <c>WithTracing</c> passes it.</param>
    /// <param name="configure">
    /// Sets <see cref="OtlpExporterOptions"/>, after the <c>OTEL_EXPORTER_OTLP_*</c> settings in the
    /// application's configuration. Leave it out to use those settings alone.
    /// </param>
    /// <returns><paramref name="builder"/>.</returns>
    /// <remarks>
    /// <para>
    /// Spans are held in the instance's memory until a span with no parent in the application
    /// ends, which for a web request is the request's own span. They are then encoded and sent
    /// while the client already has the response. Work remaining after response completion uses
    /// the instance's background allowance. <see cref="TraceExport.FlushAsync"/> sends them sooner.
    /// </para>
    /// <para>
    /// Spans still held when the instance is stopped are lost, and an export the endpoint refuses
    /// is logged and not retried. <see cref="OtlpExporterOptions.ExportProcessorType"/> is ignored;
    /// <see cref="OtlpExporterOptions.BatchExportProcessorOptions"/> sets how many spans are held
    /// and how many go in one export.
    /// </para>
    /// </remarks>
    /// <exception cref="InvalidOperationException">It is called a second time for the same application.</exception>
    /// <example>
    /// <code>
    /// builder.Services.AddOpenTelemetry()
    ///     .WithTracing(tracing => tracing
    ///         .AddAspNetCoreInstrumentation()
    ///         .AddIsoplaneOtlpExporter());
    /// </code>
    /// </example>
    public static TracerProviderBuilder AddIsoplaneOtlpExporter(
        this TracerProviderBuilder builder, Action<OtlpExporterOptions>? configure = null)
    {
        ArgumentNullException.ThrowIfNull(builder);
        builder.ConfigureServices(services => services.TryAddSingleton(provider =>
            new TraceExport(provider.GetService<ILogger<TraceExport>>() ?? NullLogger<TraceExport>.Instance)));
        return builder.AddProcessor(provider => provider.GetRequiredService<TraceExport>()
            .CreateProcessor(provider.GetService<IConfiguration>(), configure));
    }
}
