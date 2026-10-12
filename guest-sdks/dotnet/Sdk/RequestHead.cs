using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using Isoplane.Abi;

namespace Isoplane;

/// <summary>
/// The parts of an HTTP request other than its body: the method, the URL, the headers, and a token
/// that says when to stop.
/// </summary>
/// <remarks>
/// It is the base of the two types an <see cref="HttpHandlerAttribute"/> method can take:
/// <see cref="Request"/>, whose body is in memory, and <see cref="StreamingRequest"/>, whose body is
/// read as a stream. An application receives one of those rather than a <c>RequestHead</c> itself.
/// </remarks>
public class RequestHead
{
    /// <summary>The HTTP method, such as <c>GET</c> or <c>POST</c>. Standard methods are in upper case.</summary>
    public string Method { get; }

    /// <summary>The address the client asked for, as it was sent, including any query string.</summary>
    /// <remarks>
    /// It is often only a path and query, such as <c>/search?q=tea</c>, and has a scheme and host
    /// name only when the request carried them. To route a request, read <see cref="Path"/> and
    /// <see cref="Query"/> instead.
    /// </remarks>
    public string Url { get; }

    /// <summary>The path the client asked for, without the query string. It begins with a slash.</summary>
    /// <remarks>
    /// The path is not percent-decoded: a space arrives as <c>%20</c>. Decode a segment with
    /// <c>Uri.UnescapeDataString</c>.
    /// </remarks>
    public string Path { get; }

    /// <summary>The query string as name and value pairs, in the order they appear in the URL.</summary>
    /// <remarks>
    /// It is a list, not a dictionary, because a name can appear more than once. A name with no
    /// <c>=</c> has an empty value. Names and values are as the client sent them: they are not
    /// percent-decoded, and a <c>+</c> is not turned into a space.
    /// </remarks>
    /// <example>
    /// The value of <c>name</c>, or null when the query has none:
    /// <code>
    /// string? name = request.Query.FirstOrDefault(pair => pair.Key == "name").Value;
    /// </code>
    /// </example>
    public IReadOnlyList<KeyValuePair<string, string>> Query { get; }

    /// <summary>The request headers as name and value pairs.</summary>
    /// <remarks>
    /// It is a list, not a dictionary, because a header sent more than once appears once for each
    /// value. Compare names ignoring case. Headers that describe the connection itself, such as
    /// <c>Connection</c> and <c>Transfer-Encoding</c>, are not included.
    /// </remarks>
    /// <example>
    /// The request's content type, or null when it has none:
    /// <code>
    /// string? contentType = request.Headers
    ///     .FirstOrDefault(header => header.Key.Equals("content-type", StringComparison.OrdinalIgnoreCase))
    ///     .Value;
    /// </code>
    /// </example>
    public IReadOnlyList<KeyValuePair<string, string>> Headers { get; }

    /// <summary>
    /// Cancelled when the request should stop: the client has gone away, or the request has used up
    /// its running time.
    /// </summary>
    /// <remarks>
    /// Pass it to the asynchronous calls the handler makes, so that work nobody is waiting for stops.
    /// <see cref="Ctx.OccupancyBudgetMs"/> describes the running time a request is allowed.
    /// </remarks>
    public CancellationToken Aborted { get; }

    internal RequestHead(DispatchRecord record)
    {
        Aborted = Isoplane.Runtime.FutureTable.Aborted;
        Method = record.Method;
        Url = record.Url;
        Path = record.Path;
        Query = Array.AsReadOnly(record.Query);
        Headers = Array.AsReadOnly(record.Headers);
    }
}

/// <summary>
/// An HTTP request whose body is read as a stream while it is still arriving. Declare it as the
/// parameter of an <see cref="HttpHandlerAttribute"/> method, in place of <see cref="Request"/>, to
/// accept uploads too large to hold in memory.
/// </summary>
/// <remarks>
/// The handler can start before the body has arrived. The method, path, query, headers and
/// cancellation token are inherited from <see cref="RequestHead"/>.
/// </remarks>
/// <example>
/// <code>
/// [HttpHandler]
/// public static async Task&lt;Response&gt; HandleAsync(StreamingRequest request)
/// {
///     long length = 0;
///     var buffer = new byte[64 * 1024];
///     int read;
///     while ((read = await request.Body.ReadAsync(buffer, request.Aborted)) != 0)
///         length += read;
///     return Response.Text($"Received {length} bytes");
/// }
/// </code>
/// </example>
public sealed class StreamingRequest : RequestHead
{
    /// <summary>The request body as a read-only, forward-only stream.</summary>
    /// <remarks>
    /// <para>
    /// Read it with <c>ReadAsync</c> or <c>CopyToAsync</c>; a read returns 0 at the end of the body.
    /// The synchronous <c>Read</c> throws <c>NotSupportedException</c>, and so do <c>Length</c>,
    /// <c>Position</c> and <c>Seek</c>. Await each read before starting the next.
    /// </para>
    /// <para>
    /// A cancellation token passed to a read is checked before the read begins; a read already in
    /// progress finishes first. A read throws <see cref="BindingException"/> when the body cannot be
    /// delivered.
    /// </para>
    /// <para>
    /// The handler need not read the whole body. Content it has not read when it returns its response
    /// is not read at all.
    /// </para>
    /// </remarks>
    public Stream Body { get; }

    /// <summary>
    /// Whether the request can carry a body. It is false for a request sent without one, as a
    /// <c>GET</c> usually is.
    /// </summary>
    /// <remarks>
    /// True does not mean the body has content: an upload whose length the client did not announce
    /// can still turn out to be empty. When it is false, <see cref="Body"/> is an empty stream.
    /// </remarks>
    public bool CanHaveBody { get; }

    internal StreamingRequest(DispatchRecord record, Stream body, bool canHaveBody) : base(record)
    {
        Body = body;
        CanHaveBody = canHaveBody;
    }
}
