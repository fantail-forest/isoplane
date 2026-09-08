using System;
using System.Collections.Generic;
using Wisp.Abi;

namespace Wisp;

/// <summary>An HTTP request delivered by the host, for any HTTP method.</summary>
public sealed class Request
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

    /// <summary>The buffered request body, without text decoding; empty when absent.</summary>
    public ReadOnlyMemory<byte> Body { get; }

    /// <summary>Creates an HTTP request from a decoded host dispatch.</summary>
    public static Request FromDispatch(DispatchRecord record)
    {
        if (record.Kind != DispatchKind.Http) throw new PostcardException();
        return new Request(record);
    }

    /// <summary>Cancelled when the host delivers this dispatch's cancellation wake.</summary>
    public System.Threading.CancellationToken Aborted { get; }

    internal Request(DispatchRecord record)
    {
        Aborted = Wisp.Runtime.FutureTable.Aborted;
        Method = record.Method;
        Url = record.Url;
        Path = record.Path;
        Query = Array.AsReadOnly(record.Query);
        Headers = Array.AsReadOnly(record.Headers);
        Body = record.Body;
    }
}
