using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane.Runtime;

/// <summary>
/// One dispatch in progress on this instance. Each slice runs one dispatch:
/// its continuations queue on its own synchronization context, and the status
/// a slice returns is its dispatch's.
/// </summary>
internal sealed class GuestDispatch
{
    private static readonly Dictionary<ulong, GuestDispatch> InProgress = new();

    internal readonly ulong Id;
    internal readonly DispatchKind Kind;
    internal readonly ulong CancelFuture;
    internal readonly IsoplaneSynchronizationContext Context;
    private readonly CancellationTokenSource? _cancellation;
    internal Task<Response>? Root;
    /// <summary>A slice of this dispatch has been asked for and has not begun.</summary>
    private bool _sliceRequested;

    private GuestDispatch(DispatchRecord record)
    {
        Id = record.DispatchId;
        Kind = record.Kind;
        CancelFuture = record.CancelFuture;
        Context = new IsoplaneSynchronizationContext();
        _cancellation = record.CancelFuture == 0 ? null : new CancellationTokenSource();
    }

    /// <summary>The dispatch the current slice runs, or none outside a dispatch.</summary>
    internal static GuestDispatch? Current { get; private set; }

    /// <summary>Cancelled when the host cancels the dispatch.</summary>
    internal CancellationToken Aborted => _cancellation?.Token ?? CancellationToken.None;

    /// <summary>Starts a dispatch and makes it current.</summary>
    internal static GuestDispatch Begin(DispatchRecord record)
    {
        var dispatch = new GuestDispatch(record);
        InProgress[dispatch.Id] = dispatch;
        DispatchPartition.Open(dispatch.Id);
        dispatch.Enter();
        return dispatch;
    }

    /// <summary>The dispatch a wake names, made current; null if it is not in progress.</summary>
    internal static GuestDispatch? Resume(ulong id)
    {
        if (!InProgress.TryGetValue(id, out var dispatch)) return null;
        dispatch.Enter();
        return dispatch;
    }

    /// <summary>
    /// Begins a slice for a dispatch this instance has finished but the host
    /// keeps open for work it left behind, which now belongs to the instance.
    /// </summary>
    internal static void EnterFinished()
    {
        Current = null;
        DispatchPartition.EnterInstance();
        if (OperatingSystem.IsWasi())
            SynchronizationContext.SetSynchronizationContext(IsoplaneSynchronizationContext.Instance);
    }

    private void Enter()
    {
        Current = this;
        _sliceRequested = false;
        DispatchPartition.Enter(Id);
        // Only a guest drains these queues; tooling and tests run the SDK on a
        // desktop runtime, where nothing would run a continuation posted here.
        if (OperatingSystem.IsWasi()) SynchronizationContext.SetSynchronizationContext(Context);
    }

    /// <summary>Delivers the host's cancellation; false if the future is not a cancellation.</summary>
    internal static bool Cancel(ulong future)
    {
        foreach (var dispatch in InProgress.Values)
        {
            if (dispatch.CancelFuture != future) continue;
            dispatch._cancellation?.Cancel();
            return true;
        }
        return false;
    }

    /// <summary>
    /// Asks the host for a slice of every other dispatch that has work this
    /// slice made ready, or whose result is waiting to be reported.
    /// </summary>
    internal static void RequestSlicesForOthers()
    {
        foreach (var dispatch in InProgress.Values)
        {
            if (dispatch == Current || dispatch._sliceRequested) continue;
            if (!dispatch.Context.HasWork && !DispatchPartition.HasWaiting(dispatch.Id)
                && dispatch.Root is not { IsCompleted: true }) continue;
            dispatch._sliceRequested = true;
            H.DispatchReady((long)dispatch.Id);
        }
    }

    /// <summary>
    /// Ends the dispatch: its cancellation goes with it, and work it left
    /// behind, including host futures it still waits on, continues as the
    /// instance's.
    /// </summary>
    internal void End()
    {
        InProgress.Remove(Id);
        DispatchPartition.Close(Id);
        Context.HandOver(IsoplaneSynchronizationContext.Instance);
        _cancellation?.Dispose();
        if (Current == this)
        {
            Current = null;
            if (OperatingSystem.IsWasi())
                SynchronizationContext.SetSynchronizationContext(IsoplaneSynchronizationContext.Instance);
        }
    }
}
