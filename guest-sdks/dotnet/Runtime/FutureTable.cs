using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Wisp.Abi;

namespace Wisp.Runtime;

/// <summary>
/// The other half of the wisp scheduler: the map from host future ids to
/// <see cref="TaskCompletionSource{TResult}"/> slots.
///
/// Every entry is a promise the HOST holds the trigger for. The guest owns no
/// timer, no socket and no thread — only this dictionary, which lives in linear
/// memory and therefore survives a snapshot along with everything else.
/// </summary>
internal static class FutureTable
{
    private static readonly Dictionary<ulong, TaskCompletionSource<ResultRecord>> Pending = new();

    /// <summary>The guest's own ceiling on simultaneous pending futures. The
    /// host enforces the plan's <c>pending_futures_per_dispatch</c> as well;
    /// this one makes a runaway fan-out
    /// <see cref="TrapCode.TooManyFutures"/> rather than a dictionary that
    /// grows until the cell dies.</summary>
    internal const int MaxPending = 128;

    internal static int Count => Pending.Count;

    /// <summary>
    /// Registers a future id returned by an import and yields the task the
    /// guest awaits.
    ///
    /// <c>RunContinuationsAsynchronously</c> is not an optimisation here, it
    /// is a correctness requirement: without it <see cref="Complete"/> would
    /// run the awaiting continuation inline, on the stack of <c>wisp_wake</c>,
    /// beneath the host's own frame. It would still be correct, but the
    /// scheduler would no longer be the single place continuations run, and
    /// the "queue empty implies stack empty" argument above would stop being
    /// checkable in one place.
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
    /// Delivers a completion from <c>wisp_wake</c>.
    ///
    /// Wake order for distinct futures is unspecified, so this must not care
    /// which one arrives. An id we do not know is not an error — the host may
    /// complete a future the guest already abandoned.
    /// </summary>
    internal static bool Complete(long futureId, ResultRecord result)
    {
        ulong id = (ulong)futureId;
        if (!Pending.Remove(id, out var tcs)) return false;
        tcs.SetResult(result);
        return true;
    }

    internal static void Reset() => Pending.Clear();
}

internal sealed class TooManyFuturesException : Exception { }
