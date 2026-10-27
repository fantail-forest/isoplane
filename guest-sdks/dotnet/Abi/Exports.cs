using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Isoplane;
using Isoplane.Runtime;

namespace Isoplane.Abi;

/// <summary>
/// Managed bodies behind the native Mono entrypoints, parameterized by the
/// application's router. Handler failures end their request; runtime and
/// startup failures trap the instance.
/// </summary>
public static unsafe class Exports<TRouter>
    where TRouter : IIsoplaneRouter, new()
{
    private static bool _booted;
    private static bool _runtimeBooted;
    private static TRouter? _router;
    private static Task? _initialization;

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
    /// Starts a dispatch and returns its packed completion or pending status.
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

            var dispatch = GuestDispatch.Begin(d);

            // Before any timer can run, so those initialization left count
            // from this instance's activation.
            RuntimeTimers.CountFromActivation();

            if (dispatch.Kind != DispatchKind.Background)
            {
                using (DispatchPartition.Mark(dispatch.Id))
                    dispatch.Root = RequestDispatch.RouteAsync(_router!, d);
            }

            return Settle(dispatch);
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
    /// A host future completed, in a slice of the dispatch the wake record
    /// names. Distinct futures complete in no particular order.
    /// </summary>
    public static long Wake(long future, int ptr, int len)
    {
        try
        {
            Boot();
            byte[] bytes = TakeOwnership(ptr, len);

            WakeRecord wake;
            try
            {
                wake = WakeRecord.Decode(bytes);
            }
            catch (PostcardException)
            {
                return PollStatus.Trapped(TrapCode.Protocol);
            }

            Log(1, "isoplane_wake enter: dispatch=" + wake.Dispatch.ToString() + " future=" + future.ToString());

            if (_initialization is not null)
            {
                FutureTable.Complete(future, wake.Result);
                return SettleInitialization();
            }
            if (GuestDispatch.Resume(wake.Dispatch) is not { } dispatch)
            {
                // Instance work has its own slices after its originating request ends.
                GuestDispatch.EnterFinished();
                FutureTable.Complete(future, wake.Result);
                ManagedWork.Drain();
                GuestDispatch.RequestSlices();
                return PollStatus.Done(0);
            }
            FutureTable.Complete(future, wake.Result);
            return Settle(dispatch);
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
    /// Drains the dispatch's runnable continuations, then returns <c>pending</c>
    /// through the export if its root task is incomplete. Returning ends the
    /// slice with nothing of it left on the native stack; an incomplete root
    /// does not prove that a host future exists to advance it. Background
    /// dispatches have no root task and complete when no owned future remains.
    /// </summary>
    private static long Settle(GuestDispatch dispatch)
    {
        ManagedWork.Drain(dispatch);

        if (dispatch.Root is not { } root)
        {
            GuestDispatch.RequestSlices();
            if (FutureTable.Owned(dispatch.Id) > 0) return PollStatus.Pending();
            dispatch.End();
            return PollStatus.Done(0);
        }

        if (!root.IsCompleted)
        {
            GuestDispatch.RequestSlices();
            Log(1, "parking: pending futures=" + FutureTable.Count.ToString());
            return PollStatus.Pending();
        }

        dispatch.End();
        GuestDispatch.RequestSlices();

        if (!Activation.IsActivated)
            return Panic("instance startup", root.Exception?.GetBaseException() ?? new TaskCanceledException(root));

        if (root.IsCanceled) return PollStatus.Cancelled();

        if (root.IsFaulted)
        {
            Exception inner = root.Exception!.GetBaseException();
            foreach (Exception failure in root.Exception!.Flatten().InnerExceptions)
            {
                Exception cause = failure.GetBaseException();
                if (cause is OutOfMemoryException) return PollStatus.Trapped(TrapCode.OutOfMemory);
                if (cause is PostcardException) return PollStatus.Trapped(TrapCode.Protocol);
            }
            Log(3, "request failed: " + inner);
            return PollStatus.Failed(inner);
        }

        Response resp = root.Result;

        int handle = resp.Finish();

        Log(1, "done: status=" + resp.Status.ToString());

        return PollStatus.Done((uint)handle);
    }

    private static long SettleInitialization()
    {
        Task initialization = _initialization!;
        // Preparation has no background scheduler. Drain ready startup work under
        // the initialization budget before returning to wait for host I/O.
        do
        {
            ManagedWork.Drain();
        }
        while (!initialization.IsCompleted &&
            (IsoplaneSynchronizationContext.Instance.HasWork || DispatchPartition.HasWaiting(0)));
        if (!initialization.IsCompleted) return PollStatus.Pending();
        if (initialization.IsFaulted || initialization.IsCanceled)
            return initialization.Exception is { } error ? Panic("pre-initialization", error.GetBaseException()) : PollStatus.Trapped(TrapCode.Panic);
        _initialization = null;
        RuntimeTimers.MarkInitialized();
        CollectorTarget.Aim();
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
    /// Logs the operation, application and exception before trapping. The host
    /// reports only that the guest trapped, so this line is what tells an
    /// application author which of their own failures ended it.
    /// </summary>
    private static long Panic(string operation, Exception e)
    {
        try
        {
            Log(3, operation + " failed for " + typeof(TRouter).FullName + ": unhandled " + e.GetType().FullName + ": " + e.Message);
        }
        catch
        {
            // Diagnosis must not be what breaks the trap path.
        }
        return PollStatus.Unhandled(e);
    }

    private static long Panic(Exception e)
    {
        try
        {
            Log(3, "unhandled " + e.GetType().FullName + ": " + e.Message);
        }
        catch
        {
            // Diagnosis must not be what breaks the trap path.
        }
        return PollStatus.Unhandled(e);
    }

    /// <summary>
    /// Records entry into a managed dispatch.
    /// </summary>
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
