using System.Threading.Channels;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Server.Kestrel.Core;

namespace Isoplane.OpenTelemetry.Tests;

/// <summary>An OTLP endpoint on loopback that records each export and answers as told.</summary>
internal sealed class OtlpEndpoint : IAsyncDisposable
{
    private readonly WebApplication app;
    private readonly Channel<ReceivedExport> received = Channel.CreateUnbounded<ReceivedExport>();

    private OtlpEndpoint(WebApplication app) => this.app = app;

    internal Uri Address { get; private set; } = null!;

    /// <summary>The endpoint's reply: an HTTP status, and for gRPC the status in its trailers.</summary>
    internal (int Http, string Grpc) Reply { get; set; } = (200, "0");

    internal static async Task<OtlpEndpoint> StartAsync(bool grpc)
    {
        var builder = WebApplication.CreateSlimBuilder();
        builder.WebHost.ConfigureKestrel(kestrel => kestrel.Listen(System.Net.IPAddress.Loopback, 0,
            listen => listen.Protocols = grpc ? HttpProtocols.Http2 : HttpProtocols.Http1));
        var app = builder.Build();
        var endpoint = new OtlpEndpoint(app);
        app.Run(endpoint.AnswerAsync);
        await app.StartAsync();
        endpoint.Address = new Uri(app.Urls.Single());
        return endpoint;
    }

    /// <summary>The next export the endpoint receives.</summary>
    internal async Task<ReceivedExport> NextAsync() =>
        await received.Reader.ReadAsync().AsTask().WaitAsync(TimeSpan.FromSeconds(30));

    internal bool HasReceived => received.Reader.TryPeek(out _);

    private async Task AnswerAsync(HttpContext context)
    {
        using var body = new MemoryStream();
        await context.Request.Body.CopyToAsync(body);
        received.Writer.TryWrite(new ReceivedExport(context.Request.Path, context.Request.Protocol,
            context.Request.ContentType, body.ToArray()));
        var (http, grpc) = Reply;
        context.Response.StatusCode = http;
        if (context.Request.ContentType == "application/grpc")
        {
            context.Response.ContentType = "application/grpc";
            await context.Response.Body.WriteAsync(new byte[5]);
            context.Response.AppendTrailer("grpc-status", grpc);
        }
    }

    public async ValueTask DisposeAsync() => await app.DisposeAsync();
}

internal sealed record ReceivedExport(string Path, string Protocol, string? ContentType, byte[] Body)
{
    /// <summary>Whether the export carries a span of this name, which OTLP encodes as UTF-8.</summary>
    internal bool Carries(string spanName) =>
        Body.AsSpan().IndexOf(System.Text.Encoding.UTF8.GetBytes(spanName)) >= 0;
}
