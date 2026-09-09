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
