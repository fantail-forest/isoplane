using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using Wisp.Abi;

namespace Wisp;

/// <summary>HTTP request metadata, independent of whether the body is buffered or streamed.</summary>
public class RequestHead
{
    /// <summary>The HTTP method as received by the host.</summary>
    public string Method { get; }

    /// <summary>The request URI as received by the host; it may be relative.</summary>
    public string Url { get; }

    /// <summary>The request path, without the query string.</summary>
    public string Path { get; }

    /// <summary>Query pairs in host order, preserving duplicate names and empty values.</summary>
    public IReadOnlyList<KeyValuePair<string, string>> Query { get; }

    /// <summary>
    /// Header pairs as supplied by the host, preserving repeated values.
    /// Compare header names case-insensitively.
    /// </summary>
    public IReadOnlyList<KeyValuePair<string, string>> Headers { get; }

    /// <summary>Cancelled when the host delivers this dispatch's cancellation wake.</summary>
    public CancellationToken Aborted { get; }

    internal RequestHead(DispatchRecord record)
    {
        Aborted = Wisp.Runtime.FutureTable.Aborted;
        Method = record.Method;
        Url = record.Url;
        Path = record.Path;
        Query = Array.AsReadOnly(record.Query);
        Headers = Array.AsReadOnly(record.Headers);
    }
}

/// <summary>A request whose upload can arrive while its handler is running.</summary>
public sealed class StreamingRequest : RequestHead
{
    /// <summary>
    /// The non-seekable upload stream. Cancellation is checked before each read;
    /// an outstanding read completes through the host. Disposing prevents new reads,
    /// but the host retains the upload until dispatch settlement and does not drain
    /// unread content after the response.
    /// </summary>
    public Stream Body { get; }

    /// <summary>Whether HTTP framing permits content, including unknown-length uploads.</summary>
    public bool CanHaveBody { get; }

    internal StreamingRequest(DispatchRecord record, Stream body, bool canHaveBody) : base(record)
    {
        Body = body;
        CanHaveBody = canHaveBody;
    }
}
