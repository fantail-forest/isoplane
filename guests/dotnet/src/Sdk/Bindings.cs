using System;
using System.Threading.Tasks;
using Wisp.Abi;
using Wisp.Runtime;

namespace Wisp;

public sealed class BindingException : Exception
{
    internal BindingException(FutureError e) => WispError = (int)e;
    public int WispError { get; }
}

/// <summary>
/// The request as the handler sees it. Only the fields the PoC handler needs
/// are decoded; §12's HTTP surface is far larger and out of scope here.
/// </summary>
public sealed class Request
{
    public string Method { get; }
    public string Path { get; }
    internal Request(string method, string path) { Method = method; Path = path; }
}

/// <summary>The §2.2 response builder. Handed to the host via <c>resp_new</c>.</summary>
public sealed class Response
{
    internal ushort Status { get; }
    internal byte[] Body { get; }
    internal string ContentType { get; }

    private Response(ushort status, byte[] body, string contentType)
    {
        Status = status;
        Body = body;
        ContentType = contentType;
    }

    public static Response Text(string body, ushort status = 200) =>
        new(status, System.Text.Encoding.UTF8.GetBytes(body), "text/plain; charset=utf-8");

    public static Response Json(string json, ushort status = 200) =>
        new(status, System.Text.Encoding.UTF8.GetBytes(json), "application/json");
}

/// <summary>
/// §2.2 <c>Ctx</c>. <c>WaitUntil</c> extends the instance's life past the
/// response for fire-and-forget work, bounded by the CPU budget — exactly as
/// in Workers.
/// </summary>
public sealed class Ctx
{
    /// <summary>§8.5: coarse, and frozen for the whole dispatch slice. Code
    /// MUST NOT assume high-resolution timing.</summary>
    public long NowMs { get; }
    public uint CpuBudgetMs { get; }

    internal Ctx(long nowMs, uint cpuBudgetMs)
    {
        NowMs = nowMs;
        CpuBudgetMs = cpuBudgetMs;
    }
}
