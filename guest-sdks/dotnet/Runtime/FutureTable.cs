using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Wisp.Abi;

namespace Wisp.Runtime;

/// <summary>
/// The other half of the §4.2 wisp scheduler: the map from host future ids to
/// <see cref="TaskCompletionSource{TResult}"/> slots.
///
/// Every entry is a promise the HOST holds the trigger for (invariant 2). The
/// guest owns no timer, no socket and no thread — only this dictionary, which
/// lives in linear memory and therefore survives a snapshot along with
/// everything else.
/// </summary>
internal static class FutureTable
{
    private static readonly Dictionary<ulong, TaskCompletionSource<ResultRecord>> Pending = new();

    /// <summary>§11.1: the simultaneous-pending-futures cap. Exceeding it is
    /// <see cref="TrapCode.TooManyFutures"/>, not a queue that grows until the
    /// cell dies — the residency bound of §1.3/G2 is structural.</summary>
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
    /// §5.4: wake order for distinct futures is unspecified, so this must not
    /// care which one arrives. An id we do not know is not an error — the host
    /// may complete a future the guest already abandoned.
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
