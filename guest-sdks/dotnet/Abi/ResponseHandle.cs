using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;
using Isoplane.Runtime;

namespace Isoplane.Abi;

internal sealed class ResponseHandle
{
    private readonly int handle;
    private bool finished;

    internal ResponseHandle(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers, byte[] body, bool streaming)
    {
        var writer = new PostcardWriter(body.Length + 64);
        writer.Varint(status);
        writer.Varint((ulong)headers.Count);
        foreach (var header in headers)
        {
            writer.Str(header.Key);
            writer.Str(header.Value);
        }
        writer.Bytes(body);
        writer.Bool(streaming);
        unsafe
        {
            fixed (byte* pointer = writer.Span)
                handle = H.RespNew(pointer, writer.Span.Length);
        }
        if (handle <= 0) throw new InvalidOperationException("The host refused response creation; the dispatch may already have published response headers.");
    }

    internal async Task WriteAsync(ReadOnlyMemory<byte> bytes)
    {
        if (finished) throw new InvalidOperationException("The host response is already finished.");
        long future;
        unsafe
        {
            fixed (byte* pointer = bytes.Span)
                future = H.RespWrite(handle, pointer, bytes.Length);
        }
        var result = await FutureTable.Register(future).ConfigureAwait(true);
        if (!result.IsOk) throw new IOException($"The response body could not be sent: the host reported {result.Error}.");
    }

    internal int Finish()
    {
        if (finished) throw new InvalidOperationException("The host response is already finished.");
        finished = true;
        H.RespFinish(handle);
        return handle;
    }
}
