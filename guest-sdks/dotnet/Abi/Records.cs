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
    internal uint PendingFuturesLimit;
    internal ulong NowMs;

    // Populated for DispatchKind.Http.
    internal string Method = "";
    internal string Path = "";
    internal string Url = "";
    internal KeyValuePair<string, string>[] Query = Array.Empty<KeyValuePair<string, string>>();
    internal KeyValuePair<string, string>[] Headers = Array.Empty<KeyValuePair<string, string>>();

    // Not part of the encoded record: the runtime reads the whole body from
    // the host before it routes a request to a handler that takes it buffered.
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
                // HttpRequest { method, url, path, query, headers }
                d.Method = r.Str();
                d.Url = r.Str();
                d.Path = r.Str();
                d.Query = r.StringPairs();
                d.Headers = r.StringPairs();
                break;
            case DispatchKind.Background:
                break;
            default:
                throw new PostcardException();
        }

        d.CancelFuture = r.U64();
        d.OccupancyBudgetMs = r.U32();
        d.PendingFuturesLimit = r.U32();
        d.NowMs = r.U64();
        return d;
    }

}

/// <summary>
/// <c>isoplane-abi::result::ResultRecord</c> as delivered by <c>isoplane_wake</c>.
/// </summary>
internal sealed class ResultRecord
{
    internal bool IsOk;
    internal byte[] Payload = Array.Empty<byte>();
    internal FutureError Error;

    internal static ResultRecord Decode(ref PostcardReader r)
    {
        var kind = (ResultKind)r.Varint();
        if (kind == ResultKind.Ok) return new ResultRecord { IsOk = true, Payload = r.Bytes().ToArray() };
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
