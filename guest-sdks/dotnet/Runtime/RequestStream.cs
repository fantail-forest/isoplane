using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane.Runtime;

internal sealed class RequestStream(ReadOnlyMemory<byte> prefix, bool canHaveBody) : Stream
{
    private ReadOnlyMemory<byte> remaining = prefix;
    private bool eof = !canHaveBody;
    private bool reading;
    private bool disposed;

    public override bool CanRead => !disposed;
    public override bool CanSeek => false;
    public override bool CanWrite => false;
    public override long Length => throw new NotSupportedException("Request uploads have no seekable length.");
    public override long Position { get => throw new NotSupportedException("Request uploads are not seekable."); set => throw new NotSupportedException("Request uploads are not seekable."); }
    public override void Flush() => throw new NotSupportedException("Request uploads are read-only.");
    public override int Read(byte[] buffer, int offset, int count) => throw new NotSupportedException("Use ReadAsync for request uploads.");
    public override long Seek(long offset, SeekOrigin origin) => throw new NotSupportedException("Request uploads are not seekable.");
    public override void SetLength(long value) => throw new NotSupportedException("Request uploads are read-only.");
    public override void Write(byte[] buffer, int offset, int count) => throw new NotSupportedException("Request uploads are read-only.");

    public override Task<int> ReadAsync(byte[] buffer, int offset, int count, CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(buffer);
        ArgumentOutOfRangeException.ThrowIfNegative(offset);
        ArgumentOutOfRangeException.ThrowIfNegative(count);
        if (buffer.Length - offset < count) throw new ArgumentException("The read range exceeds the buffer.");
        return ReadAsync(buffer.AsMemory(offset, count), cancellationToken).AsTask();
    }

    public override async ValueTask<int> ReadAsync(Memory<byte> buffer, CancellationToken cancellationToken = default)
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        cancellationToken.ThrowIfCancellationRequested();
        if (buffer.IsEmpty) return 0;
        if (reading) throw new InvalidOperationException("Await the previous request read before starting another.");
        if (!remaining.IsEmpty)
        {
            int count = Math.Min(buffer.Length, remaining.Length);
            remaining[..count].CopyTo(buffer);
            remaining = remaining[count..];
            return count;
        }
        if (eof) return 0;
        reading = true;
        try
        {
            ResultRecord result = await FutureTable.Register(H.ReqBodyRead(buffer.Length));
            if (!result.IsOk) throw new IOException($"The request body could not be read: the host reported {result.Error}.");
            if (result.Payload.Length > buffer.Length) throw new IOException("The host returned more request bytes than requested.");
            result.Payload.AsMemory().CopyTo(buffer);
            eof = result.Payload.Length == 0;
            return result.Payload.Length;
        }
        finally { reading = false; }
    }

    protected override void Dispose(bool disposing)
    {
        disposed = true;
        remaining = ReadOnlyMemory<byte>.Empty;
        base.Dispose(disposing);
    }
}
