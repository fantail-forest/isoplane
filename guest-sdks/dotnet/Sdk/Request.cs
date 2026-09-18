using System;
using System.Collections.Generic;
using Isoplane.Abi;

namespace Isoplane;

/// <summary>An HTTP request delivered by the host, for any HTTP method.</summary>
public sealed class Request : RequestHead
{
    /// <summary>The buffered request body, without text decoding; empty when absent.</summary>
    public ReadOnlyMemory<byte> Body { get; }

    /// <summary>Creates an HTTP request from a decoded host dispatch.</summary>
    public static Request FromDispatch(DispatchRecord record)
    {
        if (record.Kind != DispatchKind.Http) throw new PostcardException();
        return new Request(record);
    }

    internal Request(DispatchRecord record) : base(record)
    {
        Body = record.Body;
    }
}
