using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Isoplane.Abi;
using Isoplane.Runtime;

namespace Isoplane;

/// <summary>
/// Thrown when Isoplane could not carry out an operation on the request being handled: reading from
/// <see cref="StreamingRequest.Body"/>, or writing with <see cref="ResponseStream.WriteAsync"/>.
/// </summary>
/// <remarks>
/// The request cannot continue normally once this is thrown, for example because it was cancelled.
/// Let the exception end the handler, or catch it to release what the handler holds and then stop;
/// retrying the same read or write does not help.
/// </remarks>
public sealed class BindingException : Exception
{
    internal BindingException(FutureError e) => IsoplaneError = (int)e;
    /// <summary>A number identifying why the operation failed.</summary>
    /// <remarks>
    /// The SDK defines no named constants for these numbers. Include the value when you log or report
    /// the failure.
    /// </remarks>
    public int IsoplaneError { get; }
}

/// <summary>The HTTP response an <see cref="HttpHandlerAttribute"/> method returns.</summary>
/// <remarks>
/// <para>
/// Create one with the static method that fits what you are sending:
/// </para>
/// <list type="bullet">
/// <item><description><see cref="Text"/> for plain text.</description></item>
/// <item><description><see cref="Json"/> for JSON you have already serialized to a string.</description></item>
/// <item><description><see cref="Bytes"/> for binary content with a content type, such as an image.</description></item>
/// <item><description><see cref="Raw"/> for bytes sent without a content type.</description></item>
/// <item><description><see cref="Http"/> to choose every header yourself, as a redirect or a cookie needs.</description></item>
/// <item><description><see cref="Stream"/> to send the body in pieces as it is produced, instead of building it all first.</description></item>
/// </list>
/// <para>
/// <see cref="Bytes"/>, <see cref="Raw"/> and <see cref="Http"/> keep the array they are given rather
/// than copying it, so do not change it after the call. Isoplane answers the client with status 502
/// in place of a response whose status is below 200 or whose headers are not valid HTTP.
/// </para>
/// <para>
/// An ASP.NET application does not use this type: it writes to <c>HttpContext.Response</c> as usual.
/// </para>
/// </remarks>
public sealed class Response
{
    private readonly ResponseStream? stream;

    internal Response(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers, ResponseStream stream)
        : this(status, Array.Empty<byte>(), headers) => this.stream = stream;

    internal int Finish() => stream is null
        ? new ResponseHandle(Status, Headers, Body, streaming: false).Finish()
        : stream.Finish();

    /// <summary>
    /// Starts a response whose body is sent in pieces as the handler produces them, and returns the
    /// writer for those pieces.
    /// </summary>
    /// <param name="status">The HTTP status code, such as 200.</param>
    /// <param name="headers">
    /// The response headers, one entry for each value. No header is added for you, so include
    /// <c>content-type</c>.
    /// </param>
    /// <returns>The writer for the body.</returns>
    /// <remarks>
    /// <para>
    /// Use it when the body is large, or produced over time, and the client should receive each piece
    /// without waiting for the rest. Write the pieces with <see cref="ResponseStream.WriteAsync"/>,
    /// then return the result of <see cref="ResponseStream.Complete"/> from the handler.
    /// </para>
    /// <para>
    /// The status and headers are sent when this method is called, so they cannot change afterwards,
    /// and the handler can no longer answer with a different response. Call it at most once for a
    /// request.
    /// </para>
    /// </remarks>
    /// <example>
    /// <code>
    /// [HttpHandler]
    /// public static async Task&lt;Response&gt; HandleAsync(Request request)
    /// {
    ///     var response = Response.Stream(200, [new("content-type", "text/plain; charset=utf-8")]);
    ///     for (var remaining = 3; remaining > 0; remaining--)
    ///         await response.WriteAsync(Encoding.UTF8.GetBytes($"{remaining}\n"), request.Aborted);
    ///     return response.Complete();
    /// }
    /// </code>
    /// </example>
    public static ResponseStream Stream(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers) => new(status, headers);

    internal ushort Status { get; }
    internal byte[] Body { get; }
    internal IReadOnlyList<KeyValuePair<string, string>> Headers { get; }

    private Response(ushort status, byte[] body, string? contentType)
    {
        Status = status;
        Body = body;
        Headers = contentType is null ? Array.Empty<KeyValuePair<string, string>>() :
            new[] { new KeyValuePair<string, string>("content-type", contentType) };
    }

    private Response(ushort status, byte[] body, IReadOnlyList<KeyValuePair<string, string>> headers)
    {
        Status = status;
        Body = body;
        Headers = headers;
    }

    /// <summary>Creates a response with exactly the status, body and headers you give it.</summary>
    /// <param name="status">The HTTP status code, such as 200 or 302.</param>
    /// <param name="body">The response body; pass an empty array for none.</param>
    /// <param name="headers">
    /// The response headers, one entry for each value. To send a header twice, as with two cookies,
    /// add two entries with the same name.
    /// </param>
    /// <remarks>
    /// Use it when the other methods do not set the headers you need: a redirect's <c>location</c>, a
    /// <c>set-cookie</c>, caching headers. No header is added for you, so include <c>content-type</c>
    /// when the response has a body.
    /// </remarks>
    /// <example>
    /// A redirect:
    /// <code>
    /// return Response.Http(302, [], [new("location", "/login")]);
    /// </code>
    /// </example>
    public static Response Http(ushort status, byte[] body, IReadOnlyList<KeyValuePair<string, string>> headers) =>
        new(status, body, headers);

    /// <summary>
    /// Creates a plain-text response, sent as UTF-8 with the content type
    /// <c>text/plain; charset=utf-8</c>.
    /// </summary>
    /// <param name="body">The text to send.</param>
    /// <param name="status">The HTTP status code.</param>
    public static Response Text(string body, ushort status = 200) =>
        new(status, System.Text.Encoding.UTF8.GetBytes(body), "text/plain; charset=utf-8");

    /// <summary>
    /// Creates a JSON response from text that is already JSON, sent as UTF-8 with the content type
    /// <c>application/json</c>.
    /// </summary>
    /// <param name="json">
    /// The JSON text. It is sent as it is: serialize an object first, for example with
    /// <c>JsonSerializer.Serialize</c>. The text is not checked for being valid JSON.
    /// </param>
    /// <param name="status">The HTTP status code.</param>
    /// <example>
    /// <code>
    /// return Response.Json(JsonSerializer.Serialize(greeting, AppJson.Default.Greeting));
    /// </code>
    /// </example>
    public static Response Json(string json, ushort status = 200) =>
        new(status, System.Text.Encoding.UTF8.GetBytes(json), "application/json");

    /// <summary>Creates a response from bytes, sent without a content type.</summary>
    /// <param name="body">The bytes to send. The array is kept, not copied: do not change it after the call.</param>
    /// <param name="status">The HTTP status code.</param>
    /// <remarks>
    /// A client usually needs to be told what it is receiving, so prefer <see cref="Bytes"/>. Use this
    /// for a body no client will interpret, or when the content type does not matter.
    /// </remarks>
    public static Response Raw(byte[] body, ushort status = 200) =>
        new(status, body, (string?)null);

    /// <summary>Creates a response from bytes with the content type you name, such as <c>image/png</c>.</summary>
    /// <param name="body">The bytes to send. The array is kept, not copied: do not change it after the call.</param>
    /// <param name="contentType">
    /// The value of the <c>content-type</c> header. Null sends no content type, as <see cref="Raw"/> does.
    /// </param>
    /// <param name="status">The HTTP status code.</param>
    public static Response Bytes(byte[] body, string? contentType, ushort status = 200) =>
        new(status, body, contentType);
}

/// <summary>
/// Facts about the request being handled: when it started, and how much running time it is allowed.
/// </summary>
/// <remarks>
/// Isoplane passes one to an <see cref="HttpHandlerAttribute"/> method that declares it as a second
/// parameter. A handler that needs neither value can leave the parameter out.
/// </remarks>
/// <example>
/// <code>
/// [HttpHandler]
/// public static Task&lt;Response&gt; HandleAsync(Request request, Ctx ctx)
/// {
///     var started = DateTimeOffset.FromUnixTimeMilliseconds(ctx.NowMs);
///     return Task.FromResult(Response.Text($"Started at {started:O}"));
/// }
/// </code>
/// </example>
public sealed class Ctx
{
    /// <summary>
    /// The time the request started, in milliseconds since the Unix epoch (1970-01-01T00:00:00Z).
    /// </summary>
    /// <remarks>
    /// The value does not advance while the handler runs. Convert it with
    /// <c>DateTimeOffset.FromUnixTimeMilliseconds</c>.
    /// </remarks>
    public long NowMs { get; }
    /// <summary>The running time this request is allowed, in milliseconds.</summary>
    /// <remarks>
    /// <para>
    /// Running time is the time the request spends executing. Time spent waiting for something the
    /// handler awaits, such as a database or another service, does not count. The value is the
    /// request's whole allowance, not what remains of it.
    /// </para>
    /// <para>
    /// When a request has used its allowance, <see cref="RequestHead.Aborted"/> is cancelled. The
    /// request then has a short further period in which to finish; after that Isoplane stops the
    /// instance.
    /// </para>
    /// <para>
    /// A local session allows the most the platform offers. To see how the application behaves with
    /// less, set <c>occupancyMsPerHttpDispatch</c> as
    /// <see href="https://isoplane.dev/docs/local-settings#try-the-application-under-tighter-limits">Try the application under tighter limits</see>
    /// describes.
    /// </para>
    /// </remarks>
    public uint OccupancyBudgetMs { get; }

    internal Ctx(long nowMs, uint occupancyBudgetMs)
    {
        NowMs = nowMs;
        OccupancyBudgetMs = occupancyBudgetMs;
    }
}

