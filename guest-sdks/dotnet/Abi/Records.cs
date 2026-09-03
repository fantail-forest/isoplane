using System;

namespace Wisp.Abi;

/// <summary>
/// The subset of <c>wisp-abi::dispatch::DispatchRecord</c> the SDK reads,
/// decoded in the Rust struct's field order.
/// </summary>
public sealed class DispatchRecord
{
    internal ulong DispatchId;
    internal DispatchKind Kind;
    internal ulong CancelFuture;
    internal uint CpuBudgetMs;
    internal ulong NowMs;

    // Populated for DispatchKind.Http.
    internal string Method = "";
    internal string Path = "";

    internal static DispatchRecord Decode(ReadOnlySpan<byte> bytes)
    {
        var r = new PostcardReader(bytes);
        var d = new DispatchRecord();
        d.DispatchId = r.U64();

        d.Kind = (DispatchKind)r.Varint();
        switch (d.Kind)
        {
            case DispatchKind.Http:
                // HttpRequest { method, url, path, query, headers, body }
                d.Method = r.Str();
                r.SkipBytes();          // url
                d.Path = r.Str();
                r.SkipStringPairs();    // query
                r.SkipStringPairs();    // headers
                r.SkipBytes();          // body
                break;

            default:
                throw new PostcardException();
        }

        // TraceContext { trace_id, span_id, sampled }
        r.SkipBytes();
        r.SkipBytes();
        r.Bool();

        d.CancelFuture = r.U64();
        d.CpuBudgetMs = r.U32();
        d.NowMs = r.U64();
        return d;
    }
}

/// <summary>Mirrors <c>wisp-abi::dispatch::DispatchKind</c>.</summary>
public enum DispatchKind : uint
{
    /// <summary>
    /// An inbound HTTP request. The only trigger a Handler receives.
    /// </summary>
    Http = 0,







}

/// <summary>
/// <c>wisp-abi::result::ResultRecord</c> as delivered by <c>wisp_wake</c>.
/// </summary>
internal sealed class ResultRecord
{
    internal bool IsOk;
    internal byte[] Payload = Array.Empty<byte>();
    internal FutureError Error;

    internal static ResultRecord Decode(ReadOnlySpan<byte> bytes)
    {
        var r = new PostcardReader(bytes);
        uint tag = r.U32();
        if (tag == 0) return new ResultRecord { IsOk = true, Payload = r.Bytes().ToArray() };
        return new ResultRecord { IsOk = false, Error = (FutureError)r.Varint() };
    }
}

/// <summary>Mirrors <c>wisp-abi::result::FutureError</c>.</summary>
internal enum FutureError : uint
{

    Cancelled = 0,
    Denied = 1,

    NotFound = 2,
    Io = 3,
}
