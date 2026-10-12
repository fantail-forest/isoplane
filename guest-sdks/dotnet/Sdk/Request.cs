using System;
using System.Collections.Generic;
using Isoplane.Abi;

namespace Isoplane;

/// <summary>
/// An HTTP request whose whole body is in memory before the handler runs. It is the usual parameter
/// of an <see cref="HttpHandlerAttribute"/> method.
/// </summary>
/// <remarks>
/// <para>
/// Isoplane creates the request and passes it to the handler; an application does not construct one.
/// The method, path, query, headers and cancellation token are inherited from
/// <see cref="RequestHead"/>.
/// </para>
/// <para>
/// To accept an upload too large to hold in memory, declare the handler's parameter as a
/// <see cref="StreamingRequest"/> instead. An ASP.NET application uses neither type: it reads
/// <c>HttpContext.Request</c> as usual.
/// </para>
/// </remarks>
/// <example>
/// <code>
/// [HttpHandler]
/// public static Task&lt;Response&gt; HandleAsync(Request request)
/// {
///     if (request.Method != "POST" || request.Path != "/echo")
///         return Task.FromResult(Response.Text("Not found", 404));
///     var text = Encoding.UTF8.GetString(request.Body.Span);
///     return Task.FromResult(Response.Text(text));
/// }
/// </code>
/// </example>
public sealed class Request : RequestHead
{
    /// <summary>The request body as the bytes the client sent; empty when the request has no body.</summary>
    /// <remarks>
    /// The bytes are not decoded in any way. Turn them into text with
    /// <c>Encoding.UTF8.GetString(request.Body.Span)</c>, or into an object with
    /// <c>JsonSerializer.Deserialize(request.Body.Span, ...)</c>.
    /// </remarks>
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
