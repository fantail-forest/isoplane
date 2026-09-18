using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane;

/// <summary>A response whose headers are published immediately and whose writes await host acceptance.</summary>
public sealed class ResponseStream
{
    private readonly ResponseHandle handle;
    private readonly Response response;
    private bool writing;
    private bool complete;

    internal ResponseStream(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers)
    {
        handle = new ResponseHandle(status, headers, Array.Empty<byte>(), streaming: true);
        response = new Response(status, headers, this);
    }

    /// <summary>Waits until the host accepts the entire buffer; the buffer must remain unchanged until completion.</summary>
    /// <remarks>Cancellation is checked before submission and after acknowledgement. It cannot interrupt a pending host write.</remarks>
    public async ValueTask WriteAsync(ReadOnlyMemory<byte> bytes, CancellationToken cancellationToken = default)
    {
        if (complete) throw new InvalidOperationException("The streaming response is complete.");
        if (writing) throw new InvalidOperationException("Await the previous response write before starting another.");
        cancellationToken.ThrowIfCancellationRequested();
        if (bytes.IsEmpty) return;
        writing = true;
        try
        {
            await handle.WriteAsync(bytes);
            cancellationToken.ThrowIfCancellationRequested();
        }
        finally { writing = false; }
    }

    /// <summary>Disallows further writes and returns the handler response. The host sees EOF only after the handler returns it.</summary>
    public Response Complete()
    {
        if (writing) throw new InvalidOperationException("Await the response write before completing the response.");
        complete = true;
        return response;
    }

    internal int Finish()
    {
        if (!complete || writing) throw new InvalidOperationException("The streaming response has not completed its writes.");
        return handle.Finish();
    }
}
