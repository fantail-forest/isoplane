using System.Diagnostics.CodeAnalysis;
using System.Net;
using System.Net.Http.Headers;

namespace Isoplane.OpenTelemetry;

/// <summary>An OTLP request the exporter encoded, kept to be sent after the response.</summary>
internal sealed class EncodedExport
{
    private readonly HttpMethod method;
    private readonly Version version;
    private readonly HttpVersionPolicy versionPolicy;
    private readonly KeyValuePair<string, string[]>[] headers;
    private readonly KeyValuePair<string, string[]>[] contentHeaders;
    private readonly byte[] body;
    private readonly bool grpc;

    internal EncodedExport(HttpRequestMessage request, bool grpc, CancellationToken cancellationToken)
    {
        method = request.Method;
        Uri = request.RequestUri ?? throw new ArgumentException("The exporter sent a request without a URI.", nameof(request));
        version = request.Version;
        versionPolicy = request.VersionPolicy;
        headers = Copy(request.Headers);
        contentHeaders = request.Content is null ? [] : Copy(request.Content.Headers);
        // The exporter reuses its buffer once the request returns, so the body is copied now.
        using var copy = new MemoryStream();
        request.Content?.ReadAsStream(cancellationToken).CopyTo(copy);
        body = copy.ToArray();
        this.grpc = grpc;
    }

    internal Uri Uri { get; }

    internal HttpRequestMessage ToRequest()
    {
        var request = new HttpRequestMessage(method, Uri)
        {
            Version = version,
            VersionPolicy = versionPolicy,
            Content = new ByteArrayContent(body),
        };
        foreach (var (name, values) in headers) request.Headers.TryAddWithoutValidation(name, values);
        foreach (var (name, values) in contentHeaders) request.Content.Headers.TryAddWithoutValidation(name, values);
        return request;
    }

    /// <summary>Whether the endpoint accepted the export; a gRPC endpoint answers in its status.</summary>
    internal bool Accepted(HttpResponseMessage response, [NotNullWhen(false)] out string? refusal)
    {
        if (!response.IsSuccessStatusCode)
        {
            refusal = $"HTTP {(int)response.StatusCode}";
            return false;
        }
        var status = grpc ? GrpcStatus(response) : "0";
        refusal = status == "0" ? null : $"gRPC status {status ?? "missing"}";
        return refusal is null;
    }

    // A gRPC status arrives in the trailers, or in the headers of a response with no body.
    private static string? GrpcStatus(HttpResponseMessage response) =>
        response.TrailingHeaders.TryGetValues("grpc-status", out var trailer) ? trailer.FirstOrDefault()
        : response.Headers.TryGetValues("grpc-status", out var header) ? header.FirstOrDefault()
        : null;

    private static KeyValuePair<string, string[]>[] Copy(HttpHeaders source) =>
        [.. source.Select(header => new KeyValuePair<string, string[]>(header.Key, [.. header.Value]))];
}

/// <summary>
/// The exporter's HTTP handler: it keeps each request for <see cref="TraceExport"/> to send and
/// answers at once that the endpoint accepted it, so the exporter, which cannot wait, never waits.
/// </summary>
internal sealed class EncodedExportCapture(TraceExport export, bool grpc) : HttpMessageHandler
{
    protected override HttpResponseMessage Send(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        export.Enqueue(new EncodedExport(request, grpc, cancellationToken));
        var response = new HttpResponseMessage(HttpStatusCode.OK)
        {
            RequestMessage = request,
            Version = request.Version,
            Content = new ByteArrayContent([]),
        };
        if (grpc)
        {
            response.Content.Headers.ContentType = new MediaTypeHeaderValue("application/grpc");
            response.TrailingHeaders.TryAddWithoutValidation("grpc-status", "0");
        }
        return response;
    }

    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) =>
        Task.FromResult(Send(request, cancellationToken));
}
