using System;
using System.Collections.Generic;
using System.Threading;

namespace Wisp.Runtime;

/// <summary>
/// The "wisp scheduler" of spec.md §4.2: the <see cref="SynchronizationContext"/>
/// half of the pair that owns the continuation queue.
///
/// It exists to make one thing true. When guest code writes
/// <c>await Cache.GetAsync(...)</c>, Roslyn's CPS transform has already
/// converted the method into a state machine whose locals live on the GC heap
/// — that is, in linear memory — and whose <c>MoveNext</c> <em>returns</em> at
/// the await point. The awaiter then hands its continuation to whatever
/// <see cref="SynchronizationContext.Current"/> is. If that context ran the
/// continuation inline on a host thread we would be back to hidden stacks; if
/// it were the default context it would go to the thread pool, which does not
/// exist here (§2.5.1, N4). Posting it to this queue instead means the
/// continuation is a heap object waiting to be dequeued, and the only way it
/// ever runs is a fresh call in from the host.
///
/// So: continuation queue empty + root task incomplete = the wasm stack is
/// empty and every scrap of the dispatch's state is in linear memory. That is
/// invariant 1 and invariant 3 simultaneously, and it falls out of the C#
/// language guarantee rather than out of anything WISP does at runtime.
/// </summary>
internal sealed class WispSynchronizationContext : SynchronizationContext
{
    internal static readonly WispSynchronizationContext Instance = new();

    private readonly Queue<Work> _queue = new();

    private WispSynchronizationContext() { }

    private readonly struct Work
    {
        internal readonly SendOrPostCallback Callback;
        internal readonly object? State;
        internal Work(SendOrPostCallback cb, object? state) { Callback = cb; State = state; }
    }

    public override void Post(SendOrPostCallback d, object? state) => _queue.Enqueue(new Work(d, state));

    /// <summary>
    /// §2.5.2. <c>Send</c> is synchronous-wait-for-completion, i.e. blocking,
    /// and blocking is inexpressible inside the sandbox by construction: the
    /// only thing that can complete a host future is a fresh <c>wisp_wake</c>
    /// call, which cannot arrive while we are still inside this one (§5.4
    /// single-caller rule). A guest that reaches here has smuggled a
    /// sync-over-async pattern past the analyzers; it would deadlock and then
    /// be killed at its CPU budget. Failing loudly and immediately is strictly
    /// better than burning the tenant's budget to reach the same outcome.
    /// </summary>
    public override void Send(SendOrPostCallback d, object? state) => throw new NotSupportedException();

    public override SynchronizationContext CreateCopy() => this;

    internal bool HasWork => _queue.Count > 0;

    /// <summary>
    /// Runs queued continuations until the queue drains. Each callback may
    /// enqueue more work (a chained await that completed synchronously), so
    /// this is a loop and not a for-each.
    /// </summary>
    internal void Drain()
    {
        while (_queue.Count > 0)
        {
            Work w = _queue.Dequeue();
            w.Callback(w.State);
        }
    }
}
