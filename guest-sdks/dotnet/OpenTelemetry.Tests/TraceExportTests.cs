using System.Diagnostics;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using OpenTelemetry.Exporter;
using OpenTelemetry.Trace;
using Xunit;

namespace Isoplane.OpenTelemetry.Tests;

public sealed class TraceExportTests
{
    [Fact]
    public async Task A_request_s_spans_reach_the_endpoint_once_its_own_span_ends()
    {
        await using var endpoint = await OtlpEndpoint.StartAsync(grpc: false);
        await using var tracing = Tracing.Start(options => options.Endpoint = new Uri(endpoint.Address, "/v1/traces"), protocol: OtlpExportProtocol.HttpProtobuf);

        using (var request = tracing.Source.StartActivity("request"))
        {
            using (tracing.Source.StartActivity("query")) { }
            Assert.False(endpoint.HasReceived, "spans are held while their request runs");
        }

        var export = await endpoint.NextAsync();
        Assert.Equal("/v1/traces", export.Path);
        Assert.Equal("application/x-protobuf", export.ContentType);
        Assert.True(export.Carries("request") && export.Carries("query"), "the export carries the request's spans");
    }

    [Fact]
    public async Task Flushing_sends_the_spans_of_a_request_that_is_still_running()
    {
        await using var endpoint = await OtlpEndpoint.StartAsync(grpc: false);
        await using var tracing = Tracing.Start(options => options.Endpoint = new Uri(endpoint.Address, "/v1/traces"), protocol: OtlpExportProtocol.HttpProtobuf);

        using var request = tracing.Source.StartActivity("request");
        using (tracing.Source.StartActivity("heavy-step")) { }

        Assert.True(await tracing.Export.FlushAsync());
        var export = await endpoint.NextAsync();
        Assert.True(export.Carries("heavy-step"), "the flush sends spans that have ended");
        Assert.False(export.Carries("request"), "a span still running is not sent");
    }

    [Fact]
    public async Task Flushing_reports_an_export_the_endpoint_refuses()
    {
        await using var endpoint = await OtlpEndpoint.StartAsync(grpc: false);
        endpoint.Reply = (503, "0");
        await using var tracing = Tracing.Start(options => options.Endpoint = new Uri(endpoint.Address, "/v1/traces"), protocol: OtlpExportProtocol.HttpProtobuf);

        using (tracing.Source.StartActivity("step")) { }

        Assert.False(await tracing.Export.FlushAsync());
    }

    [Theory]
    [InlineData("0", true)]
    [InlineData("14", false)]
    public async Task A_grpc_endpoint_s_status_decides_whether_it_accepted_the_export(string status, bool accepted)
    {
        await using var endpoint = await OtlpEndpoint.StartAsync(grpc: true);
        endpoint.Reply = (200, status);
        await using var tracing = Tracing.Start(options => options.Endpoint = endpoint.Address, protocol: OtlpExportProtocol.Grpc);

        using (tracing.Source.StartActivity("step")) { }

        Assert.Equal(accepted, await tracing.Export.FlushAsync());
        var export = await endpoint.NextAsync();
        Assert.Equal("/opentelemetry.proto.collector.trace.v1.TraceService/Export", export.Path);
        Assert.Equal("HTTP/2", export.Protocol);
        Assert.True(export.Carries("step"));
    }

    [Fact]
    public async Task Otlp_settings_in_the_application_s_configuration_choose_the_endpoint()
    {
        await using var endpoint = await OtlpEndpoint.StartAsync(grpc: false);
        KeyValuePair<string, string?>[] settings =
        [
            new("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.Address.ToString()),
            new("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
        ];
        var configuration = new ConfigurationBuilder().AddInMemoryCollection(settings).Build();
        try
        {
            await using var tracing = Tracing.Start(configure: null, configuration: configuration);

            using (tracing.Source.StartActivity("request")) { }

            var export = await endpoint.NextAsync();
            Assert.Equal("/v1/traces", export.Path);
            Assert.True(export.Carries("request"));
        }
        finally
        {
            foreach (var (key, _) in settings) Environment.SetEnvironmentVariable(key, null);
        }
    }

    /// <summary>An application's tracing with only <c>AddIsoplaneOtlpExporter</c> exporting its spans.</summary>
    private sealed class Tracing(ServiceProvider services, ActivitySource source) : IAsyncDisposable
    {
        internal ActivitySource Source => source;
        internal TraceExport Export => services.GetRequiredService<TraceExport>();

        internal static Tracing Start(
            Action<OtlpExporterOptions>? configure, OtlpExportProtocol? protocol = null, IConfiguration? configuration = null)
        {
            var source = new ActivitySource("Isoplane.OpenTelemetry.Tests." + Guid.NewGuid().ToString("N"));
            var collection = new ServiceCollection();
            collection.AddLogging();
            collection.AddSingleton<IConfiguration>(configuration ?? new ConfigurationBuilder().Build());
            collection.AddOpenTelemetry().WithTracing(tracing => tracing
                .AddSource(source.Name)
                .AddIsoplaneOtlpExporter(options =>
                {
                    if (protocol is { } chosen) options.Protocol = chosen;
                    configure?.Invoke(options);
                }));
            var services = collection.BuildServiceProvider();
            services.GetRequiredService<TracerProvider>();
            return new Tracing(services, source);
        }

        public async ValueTask DisposeAsync()
        {
            source.Dispose();
            await services.DisposeAsync();
        }
    }
}
