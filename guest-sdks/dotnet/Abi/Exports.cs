using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Isoplane.Durable;
using Isoplane.Runtime;

namespace Isoplane.Abi;

/// <summary>
/// The bodies of the guest's exports (<c>isoplane_alloc</c>, <c>isoplane_handle</c>,
/// <c>isoplane_wake</c>, <c>isoplane_initialize</c>, <c>isoplane_gc_hint</c>), generic
/// over what routes a dispatch. A guest's own <c>[UnmanagedCallersOnly]</c>
/// methods delegate here, since that attribute cannot sit on a generic class.
/// Nothing here throws across the boundary: an escaping exception in an
/// <c>UnmanagedCallersOnly</c> frame is a runtime abort with no trap code, so
/// every entrypoint converts to <see cref="PollStatus.Trapped"/>.
/// </summary>
public static unsafe class Exports<TRouter>
    where TRouter : IIsoplaneRouter, new()
{
    private static bool _booted;
    private static bool _runtimeBooted;
    private static TRouter? _router;
    private static Task<Response>? _root;
    private static Task? _initialization;
    private static Ctx? _ctx;

    /// <summary>Ensures guest scheduling and application state are initialized. Captured state may already contain initialized objects.</summary>
    private static void Boot()
    {
        if (_booted) return;
        BootRuntime();
        _router = new TRouter();
        _router.Boot();
        _booted = true;
    }

    /// <summary>The part of <see cref="Boot"/> that loads no application code or data.</summary>
    private static void BootRuntime()
    {
        if (_runtimeBooted) return;
        GuestConsole.Install();
        SynchronizationContext.SetSynchronizationContext(IsoplaneSynchronizationContext.Instance);
        _runtimeBooted = true;
    }

    /// <summary>
    /// The runtime-only pre-initialization entry point: the .NET runtime starts
    /// and the guest's console and synchronization context install, but no
    /// router, application package or application initialization runs. The
    /// host can capture this state once as a baseline common to every
    /// application on the same runtime, then continue with <see cref="Initialize"/>.
    /// </summary>
    public static long InitializeRuntime()
    {
        try
        {
            BootRuntime();
            return PollStatus.Done(0);
        }
        catch (Exception error)
        {
            return Panic("runtime pre-initialization", error);
        }
    }

    /// <summary>
    /// Where the host copies a dispatch or result record. Every pointer the
    /// host passes to <c>isoplane_handle</c> or <c>isoplane_wake</c> came from here,
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
            FutureTable.BeginCancellation(d.CancelFuture);

            _root = RequestDispatch.RouteAsync(_router!, d, _ctx);

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

            Log(1, "isoplane_wake enter: future=" + future.ToString());

            FutureTable.Complete(future, r);

            return _initialization is not null ? SettleInitialization() : Settle();
        }
        catch (TooManyFuturesException)
        {
            return PollStatus.Trapped(TrapCode.TooManyFutures);
        }
        catch (Exception error)
        {
            return Panic(error);
        }
    }

    /// <summary>Initializes guest state for a preparation attempt. A failed attempt may be retried; prior external effects are not undone.</summary>
    public static long Initialize()
    {
        try
        {
            Boot();
            var warm = new PostcardWriter(64);
            warm.Varint(1);
            warm.Str("isoplane");
            GC.KeepAlive(warm.Span.Length);
            _initialization ??= Isoplane.Initialization.EnterAsync();
#if ISOPLANE_ENTROPY_PROBE
            Isoplane.App.EntropyProbe.WarmLikePreInit();
#endif
            return SettleInitialization();
        }
        catch (Exception error)
        {
            return Panic("pre-initialization", error);
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
        catch (Exception error)
        {
            Abort("garbage collection", error);
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

        int handle = resp.Finish();

        Log(1, "done: status=" + resp.Status.ToString());
        return PollStatus.Done((uint)handle);
    }

    private static long SettleInitialization()
    {
        ManagedWork.Drain();
        Task initialization = _initialization!;
        if (!initialization.IsCompleted) return PollStatus.Pending();
        if (initialization.IsFaulted || initialization.IsCanceled)
            return initialization.Exception is { } error ? Panic("pre-initialization", error.GetBaseException()) : PollStatus.Trapped(TrapCode.Panic);
        _initialization = null;
        return PollStatus.Done(0);
    }

    private static void Abort(string operation, Exception error)
    {
        byte[] message = Encoding.UTF8.GetBytes(operation + " failed for " + typeof(TRouter).FullName
            + ": " + error.GetType().FullName);
        fixed (byte* pointer = message)
        {
            H.Abort(pointer, message.Length);
        }
    }

    /// <summary>
    /// Logs the operation, application and exception type before trapping.
    /// Exception types remain available with <c>UseSystemResourceKeys</c> and no stack traces.
    /// </summary>
    private static long Panic(string operation, Exception e)
    {
        try
        {
            Log(3, operation + " failed for " + typeof(TRouter).FullName + ": unhandled " + e.GetType().FullName);
        }
        catch
        {
            // Diagnosis must not be what breaks the trap path.
        }
        return PollStatus.Trapped(TrapCode.Panic);
    }

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
        Log(1, "isoplane_handle enter: " + d.Method + " " + d.Path);
    }

    /// <summary>
    /// The log import; stdout would put <c>wasi:cli/stdout</c> in the module's
    /// imports.
    /// </summary>
    private static void Log(int level, string message) => GuestLog.Emit(level, message);
}
