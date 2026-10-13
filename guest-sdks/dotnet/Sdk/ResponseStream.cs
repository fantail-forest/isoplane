using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane;

/// <summary>
/// Writes a response body in pieces, so the client receives each piece while the handler is still
/// producing the rest.
/// </summary>
/// <remarks>
/// <para>
/// <see cref="Response.Stream"/> creates one, and shows the whole pattern: write each piece with
/// <see cref="WriteAsync"/>, then return <see cref="Complete"/> from the handler.
/// </para>
/// <para>
/// The writer takes bytes. Encode text first, for example with <c>Encoding.UTF8.GetBytes</c>.
/// </para>
/// </remarks>
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

    /// <summary>Sends one piece of the body, and completes when Isoplane has accepted all of it.</summary>
    /// <param name="bytes">The bytes to send. Do not change them until the returned task completes. An empty buffer sends nothing.</param>
    /// <param name="cancellationToken">
    /// Stops the handler writing to a client that has gone away; pass <see cref="RequestHead.Aborted"/>.
    /// It is checked before the write begins and again when it finishes; a write in progress is not
    /// interrupted.
    /// </param>
    /// <remarks>
    /// Await each write before starting the next. Awaiting is what keeps the handler from producing
    /// output faster than it can be sent.
    /// </remarks>
    /// <exception cref="InvalidOperationException">
    /// <see cref="Complete"/> has been called, or the previous write has not finished.
    /// </exception>
    /// <exception cref="OperationCanceledException">The token was cancelled.</exception>
    /// <exception cref="System.IO.IOException">The piece could not be sent, for example because the request was cancelled.</exception>
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

    /// <summary>Ends the body and returns the response for the handler to return.</summary>
    /// <returns>The <see cref="Response"/> the handler must return.</returns>
    /// <remarks>
    /// Calling it is not enough: the client is told the body has ended only when the handler returns
    /// the response. No piece can be written afterwards. If the handler throws an exception instead
    /// of returning it, the body is broken off and the client sees an incomplete response, as
    /// <see cref="Response.Stream"/> describes.
    /// </remarks>
    /// <exception cref="InvalidOperationException">A write has not finished.</exception>
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
