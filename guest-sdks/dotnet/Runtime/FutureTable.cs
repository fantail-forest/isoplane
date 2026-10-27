using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane.Runtime;

/// <summary>
/// The map from host future ids to the operations awaiting them, behind the
/// gate the host's per-dispatch cap calls for: a dispatch starts as many host
/// operations as its record allows, and one started past that waits, in start
/// order, until an earlier operation of the same dispatch completes. Every
/// entry is a promise the host holds the trigger for, and the map lives in
/// linear memory, so it survives a capture with everything else.
/// </summary>
internal static class FutureTable
{
    /// <summary>
    /// The lane of work outside any request: the instance's own, which the host
    /// runs as one dispatch with this id.
    /// </summary>
    internal const ulong InstanceLane = ulong.MaxValue;

    private static readonly Dictionary<ulong, HostOperation> Pending = new();
    private static readonly Dictionary<ulong, Lane> Lanes = new();

    /// <summary>One dispatch's allowance: how many of its operations the host holds, and who waits for a slot.</summary>
    private sealed class Lane
    {
        internal int Limit;
        internal int Pending;
        internal readonly Queue<HostOperation> Waiting = new();
        internal Lane(int limit) => Limit = limit;
    }

    internal static int Count => Pending.Count;

    /// <summary>Opens a dispatch's lane with the cap its record carries; a background record sets the instance's.</summary>
    internal static void Open(ulong dispatch, uint limit)
    {
        int capped = (int)Math.Min(limit, int.MaxValue);
        if (Lanes.TryGetValue(dispatch, out var lane)) lane.Limit = capped;
        else Lanes[dispatch] = lane = new Lane(capped);
        Admit(lane);
    }

    /// <summary>
    /// Closes a dispatch's lane: what it still has pending or waiting becomes
    /// the instance's, as the host moves the futures themselves. Until a
    /// background dispatch has told the instance its own cap, the instance
    /// keeps the cap of the dispatch that left it work.
    /// </summary>
    internal static void Close(ulong dispatch)
    {
        if (dispatch == InstanceLane || !Lanes.Remove(dispatch, out var closed)) return;
        if (!Lanes.TryGetValue(InstanceLane, out var instance))
        {
            instance = new Lane(closed.Limit);
            Lanes[InstanceLane] = instance;
        }
        foreach (var operation in Pending.Values)
        {
            if (operation.LaneId == dispatch) operation.LaneId = InstanceLane;
        }
        instance.Pending += closed.Pending;
        while (closed.Waiting.TryDequeue(out var waiting))
        {
            waiting.LaneId = InstanceLane;
            instance.Waiting.Enqueue(waiting);
        }
        Admit(instance);
    }

    /// <summary>
    /// Starts a host operation for the current dispatch: now if the dispatch has
    /// room for it, otherwise once an earlier one completes. <paramref name="import"/>
    /// calls the import and returns its future id.
    /// </summary>
    internal static HostOperation Start(Func<long> import)
    {
        ulong id = GuestDispatch.Current?.Id ?? InstanceLane;
        var lane = LaneOf(id);
        var operation = new HostOperation(import, id);
        lane.Waiting.Enqueue(operation);
        Admit(lane);
        return operation;
    }

    /// <summary>Releases an abandoned operation and settles its managed wait.</summary>
    internal static void Cancel(HostOperation operation)
    {
        if (operation.Cancelled) return;
        operation.Cancelled = true;
        if (operation.Future == 0)
        {
            operation.Settle(new ResultRecord { Error = FutureError.Cancelled });
            return;
        }
        if (Pending.Remove((ulong)operation.Future))
        {
            operation.Settle(new ResultRecord { Error = FutureError.Cancelled });
            Release(operation.LaneId);
        }
        H.FutureCancel(operation.Future);
    }

    /// <summary>
    /// Delivers a completion from <c>isoplane_wake</c>. An unknown id is a future
    /// the guest already abandoned, or a wake with no result of its own.
    /// </summary>
    internal static bool Complete(long futureId, ResultRecord result)
    {
        ulong id = (ulong)futureId;
        if (id != 0 && GuestDispatch.Cancel(id)) return true;
        if (!Pending.Remove(id, out var operation)) return false;
        operation.Settle(result);
        Release(operation.LaneId);
        return true;
    }

    /// <summary>The operations the host holds for a dispatch.</summary>
    internal static int Owned(ulong dispatch) => Lanes.TryGetValue(dispatch, out var lane) ? lane.Pending : 0;

    /// <summary>The operations waiting for a dispatch to have room.</summary>
    internal static int Waiting(ulong dispatch) => Lanes.TryGetValue(dispatch, out var lane) ? lane.Waiting.Count : 0;

    private static Lane LaneOf(ulong id)
    {
        if (Lanes.TryGetValue(id, out var lane)) return lane;
        // Work outside any dispatch before one has run: nothing has told the
        // instance its cap, and nothing has handed it work either, so the
        // host's own refusal is the only gate.
        lane = new Lane(int.MaxValue);
        Lanes[id] = lane;
        return lane;
    }

    /// <summary>Starts waiting operations, in order, while the lane has room.</summary>
    private static void Admit(Lane lane)
    {
        while (lane.Pending < lane.Limit && lane.Waiting.TryDequeue(out var next))
        {
            if (next.Cancelled) continue;
            long id = next.Begin();
            if (id == 0)
            {
                next.Refuse();
                continue;
            }
            lane.Pending++;
            Pending[(ulong)id] = next;
        }
    }

    private static void Release(ulong id)
    {
        if (!Lanes.TryGetValue(id, out var lane)) return;
        lane.Pending--;
        Admit(lane);
    }
}

/// <summary>
/// One host operation of a dispatch: started, or waiting for the dispatch to
/// have room for it. <see cref="Completion"/> is what the guest awaits either
/// way; <c>RunContinuationsAsynchronously</c> keeps the continuation on the
/// scheduler's queue rather than running it inline under <c>isoplane_wake</c>'s
/// frame.
/// </summary>
internal sealed class HostOperation
{
    private readonly Func<long> _import;
    private readonly TaskCompletionSource<ResultRecord> _completion =
        new(TaskCreationOptions.RunContinuationsAsynchronously);

    internal HostOperation(Func<long> import, ulong lane)
    {
        _import = import;
        LaneId = lane;
    }

    internal ulong LaneId;
    internal bool Cancelled;

    /// <summary>The host future, once started; 0 while waiting.</summary>
    internal long Future { get; private set; }

    /// <summary>Completes with the host's result, or faults when the host refused the operation.</summary>
    internal Task<ResultRecord> Completion => _completion.Task;

    /// <summary>Abandons the operation: one waiting never starts, one started is cancelled at the host.</summary>
    internal void Cancel() => FutureTable.Cancel(this);

    internal long Begin()
    {
        Future = _import();
        return Future;
    }

    internal void Settle(ResultRecord result) => _completion.TrySetResult(result);

    /// <summary>The host answered the import with no future: the instance's own limit refused it.</summary>
    internal void Refuse() =>
        _completion.TrySetException(new IOException("The instance has no capacity for another pending operation."));
}
