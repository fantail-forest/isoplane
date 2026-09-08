using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Wisp.Runtime;

namespace Wisp.Abi;

/// <summary>
/// The bodies of the guest's exports (<c>wisp_alloc</c>, <c>wisp_handle</c>,
/// <c>wisp_wake</c>, <c>wisp_initialize</c>, <c>wisp_gc_hint</c>), generic
/// over what routes a dispatch. A guest's own <c>[UnmanagedCallersOnly]</c>
/// methods delegate here, since that attribute cannot sit on a generic class.
/// Nothing here throws across the boundary: an escaping exception in an
/// <c>UnmanagedCallersOnly</c> frame is a runtime abort with no trap code, so
/// every entrypoint converts to <see cref="PollStatus.Trapped"/>.
/// </summary>
public static unsafe class Exports<TRouter>
    where TRouter : IWispRouter, new()
{
    private static bool _booted;
    private static TRouter? _router;
    private static Task<Response>? _root;
    private static Ctx? _ctx;

    /// <summary>Ensures guest scheduling and application state are initialized. Captured state may already contain initialized objects.</summary>
    private static void Boot()
    {
        if (_booted) return;
        _booted = true;
        SynchronizationContext.SetSynchronizationContext(WispSynchronizationContext.Instance);
        _router = new TRouter();
        _router.Boot();
    }

    /// <summary>
    /// Where the host copies a dispatch or result record. Every pointer the
    /// host passes to <c>wisp_handle</c> or <c>wisp_wake</c> came from here,
    /// and those entrypoints free it once they have copied it onto the heap.
    /// </summary>
    public static int Alloc(int size, int align)
    {
        if (size <= 0) return 0;
        if (align <= 0 || (align & (align - 1)) != 0) align = 8;
        try
        {
            return (int)(nint)NativeMemory.AlignedAlloc((nuint)size, (nuint)align);
        }
        catch
        {
            return 0;
        }
    }

    private static byte[] TakeOwnership(int ptr, int len)
    {
        if (ptr == 0 || len <= 0) return Array.Empty<byte>();
        var span = new ReadOnlySpan<byte>((void*)(nint)ptr, len);
        byte[] copy = span.ToArray();
        NativeMemory.AlignedFree((void*)(nint)ptr);
        return copy;
    }

    /// <summary>
    /// A new dispatch. Returns <c>done</c>, <c>pending</c> or <c>trapped</c>.
    /// </summary>
    public static long Handle(int ptr, int len)
    {
        try
        {
            Boot();
            byte[] bytes = TakeOwnership(ptr, len);

            DispatchRecord d;
            try
            {
                d = DispatchRecord.Decode(bytes);
            }
            catch (PostcardException)
            {
                return PollStatus.Trapped(TrapCode.Protocol);
            }

            TraceDispatch(d);

            _ctx = new Ctx((long)d.NowMs, d.CpuBudgetMs);
            FutureTable.Reset();

            _root = _router!.Route(d, _ctx);

            return Settle();
        }
        catch (TooManyFuturesException)
        {
            return PollStatus.Trapped(TrapCode.TooManyFutures);
        }
        catch (PostcardException)
        {
            // From `Route`: a trigger this module has no handler for.
            return PollStatus.Trapped(TrapCode.Protocol);
        }
        catch (Exception e)
        {
            return Panic(e);
        }
    }

    /// <summary>
    /// A host future completed. Distinct futures complete in no particular
    /// order.
    /// </summary>
    public static long Wake(long future, int ptr, int len)
    {
        try
        {
            Boot();
            byte[] bytes = TakeOwnership(ptr, len);

            ResultRecord r;
            try
            {
                r = ResultRecord.Decode(bytes);
            }
            catch (PostcardException)
            {
                return PollStatus.Trapped(TrapCode.Protocol);
            }

            Log(1, "wisp_wake enter: future=" + future.ToString());

            FutureTable.Complete(future, r);

            return Settle();
        }
        catch (TooManyFuturesException)
        {
            return PollStatus.Trapped(TrapCode.TooManyFutures);
        }
        catch (Exception)
        {
            return PollStatus.Trapped(TrapCode.Panic);
        }
    }

    /// <summary>Initializes guest state for a preparation attempt. A failed attempt may be retried; prior external effects are not undone.</summary>
    public static void Initialize()
    {
        try
        {
            Boot();
            var warm = new PostcardWriter(64);
            warm.Varint(1);
            warm.Str("wisp");
            GC.KeepAlive(warm.Span.Length);
#if WISP_ENTROPY_PROBE
            Wisp.App.EntropyProbe.WarmLikePreInit();
#endif
        }
        catch
        {
            // A failed warm-up is not a failed deploy: the work happens on
            // the first dispatch instead.
        }
    }

    /// <summary>
    /// Called before a hibernation capture. A compacting collection leaves
    /// the freed tail zeroed, and zeroed pages compress out of the delta.
    /// </summary>
    public static void GcHint(int pressure)
    {
        try
        {
            GC.Collect(2, pressure >= 2 ? GCCollectionMode.Aggressive : GCCollectionMode.Forced, blocking: true, compacting: true);
        }
        catch
        {
            // A failed hint is never a correctness problem.
        }
    }

    /// <summary>
    /// Drains runnable continuations, then returns <c>pending</c> through the
    /// export if the root task is incomplete. Returning leaves no live guest
    /// invocation on the native stack; an incomplete root does not prove
    /// that a host future exists to advance it.
    /// </summary>
    private static long Settle()
    {
        ManagedWork.Drain();

        Task<Response> root = _root!;

        if (!root.IsCompleted)
        {
            Log(1, "parking: pending futures=" + FutureTable.Count.ToString());
            return PollStatus.Pending();
        }

        if (root.IsFaulted || root.IsCanceled)
        {
            _root = null;
            Exception? inner = root.Exception?.GetBaseException();
            return inner is null ? PollStatus.Trapped(TrapCode.Panic) : Panic(inner);
        }

        Response resp = root.Result;
        _root = null;

        // ResponseHead { status, headers, body, streaming }
        var w = new PostcardWriter(resp.Body.Length + 64);
        w.Varint(resp.Status);
        if (resp.ContentType is null)
        {
            w.Varint(0);
        }
        else
        {
            w.Varint(1);
            w.Str("content-type");
            w.Str(resp.ContentType);
        }
        w.Bytes(resp.Body);
        w.Bool(false);

        int handle;
        fixed (byte* p = w.Span)
        {
            handle = H.RespNew(p, w.Span.Length);
        }
        H.RespFinish(handle);

        Log(1, "done: status=" + resp.Status.ToString());
        return PollStatus.Done((uint)handle);
    }

    /// <summary>
    /// Logs the exception's type name, which is the diagnostic that survives
    /// <c>UseSystemResourceKeys</c> and no stack traces, then traps.
    /// </summary>
    private static long Panic(Exception e)
    {
        try
        {
            Log(3, "unhandled " + e.GetType().FullName);
        }
        catch
        {
            // Diagnosis must not be what breaks the trap path.
        }
        return PollStatus.Trapped(TrapCode.Panic);
    }

    /// <summary>Records entry into a managed dispatch.</summary>
    private static void TraceDispatch(DispatchRecord d)
    {
        Log(1, "wisp_handle enter: " + d.Method + " " + d.Path);
    }

    /// <summary>
    /// The log import; stdout would put <c>wasi:cli/stdout</c> in the module's
    /// imports.
    /// </summary>
    private static void Log(int level, string message)
    {
        byte[] utf8 = Encoding.UTF8.GetBytes(message);
        fixed (byte* p = utf8)
        {
            H.LogEmit(level, p, utf8.Length);
        }
    }
}
