using System;
using System.Collections.Generic;

namespace Isoplane.Abi;

/// <summary>
/// The subset of <c>isoplane-abi::dispatch::DispatchRecord</c> the SDK reads,
/// decoded in the Rust struct's field order.
/// </summary>
public sealed class DispatchRecord
{
    internal ulong DispatchId;
    internal DispatchKind Kind;
    internal ulong CancelFuture;
    internal uint OccupancyBudgetMs;
    internal ulong NowMs;

    // Populated for DispatchKind.Http.
    internal string Method = "";
    internal string Path = "";
    internal string Url = "";
    internal KeyValuePair<string, string>[] Query = Array.Empty<KeyValuePair<string, string>>();
    internal KeyValuePair<string, string>[] Headers = Array.Empty<KeyValuePair<string, string>>();
    internal byte[] Body = Array.Empty<byte>();

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
                d.Url = r.Str();
                d.Path = r.Str();
                d.Query = r.StringPairs();
                d.Headers = r.StringPairs();
                d.Body = r.Bytes().ToArray();
                break;
            case DispatchKind.Timers:
                break;
            default:
                throw new PostcardException();
        }

        // TraceContext { trace_id, span_id, sampled }
        r.SkipBytes();
        r.SkipBytes();
        r.Bool();

        d.CancelFuture = r.U64();
        d.OccupancyBudgetMs = r.U32();
        d.NowMs = r.U64();
        return d;
    }

}

/// <summary>Mirrors <c>isoplane-abi::dispatch::DispatchKind</c>.</summary>
public enum DispatchKind : uint
{
    /// <summary>
    /// An inbound HTTP request. The only trigger a Handler receives.
    /// </summary>
    Http = 0,

    /// <summary>Runs the timers that came due while the instance was idle, and nothing else.</summary>
    Timers = 1,
}

/// <summary>
/// <c>isoplane-abi::result::ResultRecord</c> as delivered by <c>isoplane_wake</c>.
/// </summary>
internal sealed class ResultRecord
{
    internal bool IsOk;
    internal byte[] Payload = Array.Empty<byte>();
    internal FutureError Error;

    internal static ResultRecord Decode(ReadOnlySpan<byte> bytes)
    {
        var r = new PostcardReader(bytes);
        return Decode(ref r);
    }

    internal static ResultRecord Decode(ref PostcardReader r)
    {
        uint tag = r.U32();
        if (tag == 0) return new ResultRecord { IsOk = true, Payload = r.Bytes().ToArray() };
        return new ResultRecord { IsOk = false, Error = (FutureError)r.Varint() };
    }
}

/// <summary>
/// <c>isoplane-abi::result::WakeRecord</c>: the dispatch a wake's slice runs,
/// and the completed future's result.
/// </summary>
internal sealed class WakeRecord
{
    internal ulong Dispatch;
    internal ResultRecord Result = new();

    internal static WakeRecord Decode(ReadOnlySpan<byte> bytes)
    {
        var r = new PostcardReader(bytes);
        ulong dispatch = r.U64();
        return new WakeRecord { Dispatch = dispatch, Result = ResultRecord.Decode(ref r) };
    }
}

/// <summary>Mirrors <c>isoplane-abi::result::FutureError</c>.</summary>
internal enum FutureError : uint
{

    Cancelled = 0,
    Denied = 1,
    NotFound = 2,
    Io = 3,
}
