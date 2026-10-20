using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane.Runtime;

/// <summary>
/// The map from host future ids to the tasks awaiting them. Every entry is a
/// promise the host holds the trigger for, and the map lives in linear
/// memory, so it survives a capture with everything else.
/// </summary>
internal static class FutureTable
{
    private static readonly Dictionary<ulong, Entry> Pending = new();

    private readonly struct Entry
    {
        internal readonly TaskCompletionSource<ResultRecord> Completion;
        internal readonly ulong Dispatch;
        internal Entry(TaskCompletionSource<ResultRecord> completion, ulong dispatch)
        {
            Completion = completion;
            Dispatch = dispatch;
        }
    }

    /// <summary>
    /// The guest's own ceiling on one dispatch's pending futures, so a runaway
    /// fan-out is a <see cref="TrapCode.TooManyFutures"/> trap rather than
    /// unbounded growth.
    /// </summary>
    internal const int MaxPending = 128;

    internal static int Count => Pending.Count;

    /// <summary>
    /// Registers a future id an import returned, for the current dispatch,
    /// and yields the task the guest awaits. <c>RunContinuationsAsynchronously</c>
    /// keeps the continuation on the scheduler's queue rather than running it
    /// inline under <c>isoplane_wake</c>'s frame.
    /// </summary>
    internal static Task<ResultRecord> Register(long futureId)
    {
        ulong id = (ulong)futureId;
        ulong dispatch = GuestDispatch.Current?.Id ?? 0;
        if (Owned(dispatch) >= MaxPending) throw new TooManyFuturesException();

        var tcs = new TaskCompletionSource<ResultRecord>(TaskCreationOptions.RunContinuationsAsynchronously);
        Pending[id] = new Entry(tcs, dispatch);
        return tcs.Task;
    }

    /// <summary>Cancels both the await and its host operation when the token is cancelled.</summary>
    internal static async Task<ResultRecord> Register(long futureId, CancellationToken cancellationToken)
    {
        var pending = Register(futureId);
        try
        {
            return await pending.WaitAsync(cancellationToken);
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
        {
            Pending.Remove((ulong)futureId);
            H.FutureCancel(futureId);
            throw;
        }
    }

    /// <summary>
    /// Delivers a completion from <c>isoplane_wake</c>. An unknown id is a future
    /// the guest already abandoned, or a wake with no result of its own.
    /// </summary>
    internal static bool Complete(long futureId, ResultRecord result)
    {
        ulong id = (ulong)futureId;
        if (id != 0 && GuestDispatch.Cancel(id)) return true;
        if (!Pending.Remove(id, out var entry)) return false;
        entry.Completion.SetResult(result);
        return true;
    }

    /// <summary>The futures a dispatch is waiting on.</summary>
    internal static int Owned(ulong dispatch)
    {
        int owned = 0;
        foreach (var entry in Pending.Values)
        {
            if (entry.Dispatch == dispatch) owned++;
        }
        return owned;
    }
}

internal sealed class TooManyFuturesException : Exception { }
