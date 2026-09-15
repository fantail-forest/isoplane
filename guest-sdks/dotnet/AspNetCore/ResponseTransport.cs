namespace Wisp.AspNetCore;

internal abstract class ResponseTransport
{
    internal abstract void Start(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers);
    internal abstract ValueTask WriteAsync(ReadOnlyMemory<byte> bytes, CancellationToken cancellationToken);
    internal abstract Response Complete();
}

internal sealed class HostResponseTransport : ResponseTransport
{
    private ResponseStream? stream;
    internal override void Start(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers) =>
        stream = Response.Stream(status, headers);
    internal override ValueTask WriteAsync(ReadOnlyMemory<byte> bytes, CancellationToken cancellationToken) =>
        (stream ?? throw new InvalidOperationException("The response headers have not started.")).WriteAsync(bytes, cancellationToken);
    internal override Response Complete() =>
        (stream ?? throw new InvalidOperationException("The response headers have not started.")).Complete();
}

internal sealed class WarmupResponseTransport : ResponseTransport
{
    private ushort? status;
    private IReadOnlyList<KeyValuePair<string, string>> headers = [];

    internal override void Start(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers)
    {
        this.status = status;
        this.headers = headers;
    }

    internal override ValueTask WriteAsync(ReadOnlyMemory<byte> bytes, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return ValueTask.CompletedTask;
    }

    internal override Response Complete()
    {
        var result = status ?? throw new InvalidOperationException("The warmup response has not started.");
        if (result is < 200 or >= 300)
            throw new HttpRequestException($"Application warmup returned HTTP {result}.", null, (System.Net.HttpStatusCode)result);
        return Response.Http(result, [], headers);
    }
}
