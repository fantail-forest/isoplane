using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using System.Threading;
using Wisp.Abi;

namespace Wisp.Runtime;

/// <summary>
/// The map from host future ids to the tasks awaiting them. Every entry is a
/// promise the host holds the trigger for, and the map lives in linear
/// memory, so it survives a capture with everything else.
/// </summary>
internal static class FutureTable
{
    private static readonly Dictionary<ulong, TaskCompletionSource<ResultRecord>> Pending = new();

    /// <summary>
    /// The guest's own ceiling on pending futures, so a runaway fan-out is a
    /// <see cref="TrapCode.TooManyFutures"/> trap rather than unbounded
    /// growth.
    /// </summary>
    internal const int MaxPending = 128;

    private static ulong cancellationId;
    private static CancellationTokenSource? cancellation;

    internal static CancellationToken Aborted => cancellation?.Token ?? CancellationToken.None;

    internal static void BeginCancellation(ulong id)
    {
        cancellationId = id;
        cancellation = id == 0 ? null : new CancellationTokenSource();
    }

    internal static int Count => Pending.Count;

    /// <summary>
    /// Registers a future id an import returned and yields the task the
    /// guest awaits. <c>RunContinuationsAsynchronously</c> keeps the
    /// continuation on the scheduler's queue rather than running it inline
    /// under <c>wisp_wake</c>'s frame.
    /// </summary>
    internal static Task<ResultRecord> Register(long futureId)
    {
        ulong id = (ulong)futureId;
        if (Pending.Count >= MaxPending) throw new TooManyFuturesException();

        var tcs = new TaskCompletionSource<ResultRecord>(TaskCreationOptions.RunContinuationsAsynchronously);
        Pending[id] = tcs;
        return tcs.Task;
    }

    /// <summary>
    /// Delivers a completion from <c>wisp_wake</c>. An unknown id is a future
    /// the guest already abandoned.
    /// </summary>
    internal static bool Complete(long futureId, ResultRecord result)
    {
        ulong id = (ulong)futureId;
        if (id != 0 && id == cancellationId)
        {
            cancellation!.Cancel();
            return true;
        }
        if (!Pending.Remove(id, out var tcs)) return false;
        tcs.SetResult(result);
        return true;
    }

    internal static void Reset()
    {
        Pending.Clear();
        cancellation?.Dispose();
        cancellation = null;
        cancellationId = 0;
    }
}

internal sealed class TooManyFuturesException : Exception { }
