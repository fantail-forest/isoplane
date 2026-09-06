using System;
using System.Threading.Tasks;
using Wisp.Abi;
using Wisp.Runtime;

namespace Wisp;

/// <summary>
/// A capability call the host refused or could not complete;
/// <see cref="WispError"/> says which.
/// </summary>
public sealed class BindingException : Exception
{
    internal BindingException(FutureError e) => WispError = (int)e;
    /// <summary>
    /// The <c>wisp-abi::result::FutureError</c> discriminant the host returned.
    /// </summary>
    public int WispError { get; }
}

/// <summary>A response: a status, an optional content type and a body.</summary>
public sealed class Response
{
    internal ushort Status { get; }
    internal byte[] Body { get; }
    /// <summary>
    /// <c>null</c> encodes an empty header list, which is what a Durable
    /// Instance reply carries.
    /// </summary>
    internal string? ContentType { get; }

    private Response(ushort status, byte[] body, string? contentType)
    {
        Status = status;
        Body = body;
        ContentType = contentType;
    }

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
    /// Bytes with no headers, the shape a Durable Instance reply takes. The
    /// array is handed to the host as is, so a reused buffer must be copied
    /// first.
    /// </summary>
    public static Response Raw(byte[] body, ushort status = 200) =>
        new(status, body, null);

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

