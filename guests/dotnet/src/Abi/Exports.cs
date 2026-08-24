using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Wisp.App;
using Wisp.Runtime;

namespace Wisp.Abi;

/// <summary>
/// Core WebAssembly entrypoints for host allocation, dispatch, wake and GC hints.
/// Dispatch and wake convert managed failures to protocol trap results.
/// </summary>
public static unsafe class Exports
{
    private static bool _booted;
    private static Api? _app;
    private static Task<Response>? _root;
    private static Ctx? _ctx;

    /// <summary>Ensures guest scheduling and application state are initialized. Captured state may already contain initialized objects.</summary>
    private static void Boot()
    {
        if (_booted) return;
        _booted = true;
        SynchronizationContext.SetSynchronizationContext(WispSynchronizationContext.Instance);
        _app = new Api();
    }

    // -----------------------------------------------------------------------
    // wisp_alloc(size, align) -> ptr
    // -----------------------------------------------------------------------

    /// <summary>
    /// Host-to-guest data enters via <c>wisp_alloc</c> + copy. No host
    /// pointer ever enters guest memory, so this hands back an offset into
    /// our own linear memory and nothing else.
    ///
    /// The contract with the host is that every pointer passed to
    /// <c>wisp_handle</c> / <c>wisp_wake</c> came from here, because those
    /// entrypoints free it once they have copied it onto the GC heap.
    /// </summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_alloc")]
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

    // -----------------------------------------------------------------------
    // wisp_handle(ptr, len) -> poll-status
    // -----------------------------------------------------------------------

    /// <summary>
    /// A new top-level work item. Returns <c>done</c>, <c>pending</c> or
    /// <c>trapped</c>.
    /// </summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_handle")]
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

            Log(1, "wisp_handle enter: " + d.Method + " " + d.Path);

            _ctx = new Ctx((long)d.NowMs, d.CpuBudgetMs);
            FutureTable.Reset();

            // Kicking the handler off runs it synchronously up to its first
            // incomplete await, exactly as .NET always has.
            _root = _app!.HandleAsync(new Request(d.Method, d.Path), _ctx);

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

    // -----------------------------------------------------------------------
    // wisp_wake(future, ptr, len) -> poll-status
    // -----------------------------------------------------------------------

    /// <summary>
    /// A host future completed. Delivery order for distinct futures is
    /// unspecified and the guest must not assume one — the .NET Task model
    /// already does not.
    /// </summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_wake")]
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

            // Completing the TCS does not run the continuation here: it Posts
            // it to the wisp scheduler's queue, which Settle() then drains.
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

    // -----------------------------------------------------------------------
    // wisp_gc_hint(pressure)
    // -----------------------------------------------------------------------

    /// <summary>
    /// Requests a best-effort compacting collection; pressure of two or more selects
    /// aggressive collection. This does not guarantee that unused memory is zeroed.
    /// </summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_gc_hint")]
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

    // -----------------------------------------------------------------------
    // The one place a dispatch can end.
    // -----------------------------------------------------------------------

    /// <summary>
    /// Drains runnable continuations and reports the root task status.
    /// An incomplete root returns pending; this does not prove it has a host wake source.
    /// </summary>
    private static long Settle()
    {
        WispSynchronizationContext.Instance.Drain();

        Task<Response> root = _root!;

        if (!root.IsCompleted)
        {
            Log(1, "parking: pending futures=" + FutureTable.Count.ToString());
            return PollStatus.Pending();
        }

        if (root.IsFaulted || root.IsCanceled)
        {
            _root = null;
            return PollStatus.Trapped(TrapCode.Panic);
        }

        Response resp = root.Result;
        _root = null;

        // ResponseHead { status: u16, headers: Vec<(String,String)>, body,
        //                streaming: bool }
        var w = new PostcardWriter(resp.Body.Length + 64);
        w.Varint(resp.Status);
        w.Varint(1);
        w.Str("content-type");
        w.Str(resp.ContentType);
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
    /// Emits a UTF-8 message through the host logging import.
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
