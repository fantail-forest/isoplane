namespace Wisp.AspNetCore.Tests;

internal sealed class RecordingResponseTransport : ResponseTransport
{
    internal ushort Status;
    internal IReadOnlyList<KeyValuePair<string, string>> Headers = [];
    internal readonly MemoryStream Bytes = new();
    internal readonly TaskCompletionSource Started = new();
    internal Func<ReadOnlyMemory<byte>, CancellationToken, ValueTask>? Writing;
    internal override void Start(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers)
    {
        Status = status;
        Headers = headers;
        Started.SetResult();
    }
    internal override async ValueTask WriteAsync(ReadOnlyMemory<byte> bytes, CancellationToken cancellationToken)
    {
        if (Writing is not null) await Writing(bytes, cancellationToken);
        await Bytes.WriteAsync(bytes, cancellationToken);
    }
    internal override Response Complete() => Response.Http(Status, Bytes.ToArray(), Headers);
}
