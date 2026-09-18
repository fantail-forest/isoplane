using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Isoplane.Abi;
using Isoplane.Runtime;

namespace Isoplane;

/// <summary>
/// A capability call the host refused or could not complete;
/// <see cref="IsoplaneError"/> says which.
/// </summary>
public sealed class BindingException : Exception
{
    internal BindingException(FutureError e) => IsoplaneError = (int)e;
    /// <summary>
    /// The numeric error code returned by the host for the failed capability call.
    /// </summary>
    public int IsoplaneError { get; }
}

/// <summary>A buffered or completed streaming response.</summary>
public sealed class Response
{
    private readonly ResponseStream? stream;

    internal Response(ushort status, IReadOnlyList<KeyValuePair<string, string>> headers, ResponseStream stream)
        : this(status, Array.Empty<byte>(), headers) => this.stream = stream;

    internal int Finish() => stream is null
        ? new ResponseHandle(Status, Headers, Body, streaming: false).Finish()
        : stream.Finish();

    /// <summary>Publishes response headers and creates a writer. Return its completed response from the handler.</summary>
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

    /// <summary>Creates a buffered response preserving repeated header values.</summary>
    public static Response Http(ushort status, byte[] body, IReadOnlyList<KeyValuePair<string, string>> headers) =>
        new(status, body, headers);

    /// <summary>
    /// A UTF-8 text response, with <c>text/plain; charset=utf-8</c>.
    /// </summary>
    public static Response Text(string body, ushort status = 200) =>
        new(status, System.Text.Encoding.UTF8.GetBytes(body), "text/plain; charset=utf-8");

    /// <summary>
    /// A UTF-8 JSON response, with <c>application/json</c>. The body is not validated.
    /// </summary>
    public static Response Json(string json, ushort status = 200) =>
        new(status, System.Text.Encoding.UTF8.GetBytes(json), "application/json");

    /// <summary>
    /// Returns bytes without a Content-Type header. Copy a shared buffer before
    /// passing it here if other code might modify it before the response completes.
    /// </summary>
    public static Response Raw(byte[] body, ushort status = 200) =>
        new(status, body, (string?)null);

    /// <summary>
    /// Bytes with an explicit content type, for a reply a client has to be
    /// told the type of.
    /// </summary>
    public static Response Bytes(byte[] body, string? contentType, ushort status = 200) =>
        new(status, body, contentType);
}

/// <summary>
/// The per-dispatch context.
/// </summary>
public sealed class Ctx
{
    /// <summary>
    /// The clock at dispatch start, in milliseconds, frozen for the slice.
    /// </summary>
    public long NowMs { get; }
    /// <summary>
    /// CPU milliseconds this dispatch may use before it is killed.
    /// </summary>
    public uint CpuBudgetMs { get; }

    internal Ctx(long nowMs, uint cpuBudgetMs)
    {
        NowMs = nowMs;
        CpuBudgetMs = cpuBudgetMs;
    }
}

