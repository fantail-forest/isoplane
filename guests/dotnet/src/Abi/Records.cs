using System;

namespace Wisp.Abi;

/// <summary>
/// The subset of <c>wisp-abi::dispatch::DispatchRecord</c> the guest handler
/// reads. Fields are decoded in the Rust struct's declaration
/// order because postcard writes structs as bare, unframed field sequences —
/// the order IS the format.
/// </summary>
internal sealed class DispatchRecord
{
    internal ulong DispatchId;
    internal DispatchKind Kind;
    /// <summary>The host cancels by completing this future; the guest
    /// observes it at its next await. It is never an entrancy.</summary>
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
                // Every other trigger is one this guest does not export a
                // handler for. A host that sends one is misrouting, and that
                // must surface as Protocol rather than as silent misbehaviour.
                throw new PostcardException();
        }

        // TraceContext { trace_id, span_id, sampled } (W3C propagation)
        r.SkipBytes();
        r.SkipBytes();
        r.Bool();

        d.CancelFuture = r.U64();
        d.CpuBudgetMs = r.U32();
        d.NowMs = r.U64();
        return d;
    }
}

/// <summary>Discriminant order of <c>wisp-abi::dispatch::DispatchKind</c>.</summary>
internal enum DispatchKind : uint
{
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

/// <summary>Error discriminants in the Rust guest host-interface contract.</summary>
internal enum FutureError : uint
{

    Timeout = 0,
    Cancelled = 1,
    Denied = 2,

    NotFound = 3,
    Io = 4,
}
