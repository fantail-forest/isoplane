using System;
using System.Collections.Generic;
using System.Threading;

namespace Isoplane.Runtime;

/// <summary>
/// Queues continuations posted through Isoplane's synchronization context in
/// guest memory. Each dispatch has its own, so a continuation runs in a slice
/// of the dispatch that awaited; <see cref="Instance"/> serves code outside any
/// dispatch. The export driver drains the current dispatch's queue during each
/// slice, then returns when no runnable work remains. This scheduler does not
/// intercept awaiters that bypass context capture or prove that an incomplete
/// task has a completion source.
/// </summary>
internal sealed class IsoplaneSynchronizationContext : SynchronizationContext
{
    internal static readonly IsoplaneSynchronizationContext Instance = new();

    private readonly Queue<Work> _queue = new();

    private readonly struct Work
    {
        internal readonly SendOrPostCallback Callback;
        internal readonly object? State;
        internal Work(SendOrPostCallback cb, object? state) { Callback = cb; State = state; }
    }

    public override void Post(SendOrPostCallback d, object? state) => _queue.Enqueue(new Work(d, state));

    /// <summary>
    /// <c>Send</c> blocks, and nothing can complete a host future during a
    /// slice; a sync-over-async guest would spin to its CPU
    /// budget, so it fails here instead.
    /// </summary>
    public override void Send(SendOrPostCallback d, object? state) => throw new NotSupportedException();

    public override SynchronizationContext CreateCopy() => this;

    internal bool HasWork => _queue.Count > 0;

    /// <summary>
    /// Runs queued continuations until the queue drains, including any they
    /// enqueue.
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
