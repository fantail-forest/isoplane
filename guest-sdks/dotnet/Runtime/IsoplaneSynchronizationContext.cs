using System;
using System.Collections.Generic;
using System.Threading;

namespace Isoplane.Runtime;

/// <summary>
/// Queues continuations posted through Isoplane's synchronization context in
/// guest memory. Each dispatch has its own, so a continuation runs in a slice
/// of the dispatch that awaited; <see cref="Instance"/> serves code outside any
/// dispatch, and takes over a dispatch's continuations once it ends. The export
/// driver drains the current dispatch's queue during each slice, then returns
/// when no runnable work remains. This scheduler does not
/// intercept awaiters that bypass context capture or prove that an incomplete
/// task has a completion source.
/// </summary>
internal sealed class IsoplaneSynchronizationContext : SynchronizationContext
{
    internal static readonly IsoplaneSynchronizationContext Instance = new();

    private readonly Queue<Work> _queue = new();
    private IsoplaneSynchronizationContext? _successor;

    private readonly struct Work
    {
        internal readonly SendOrPostCallback Callback;
        internal readonly object? State;
        internal Work(SendOrPostCallback cb, object? state) { Callback = cb; State = state; }
    }

    public override void Post(SendOrPostCallback d, object? state)
    {
        if (_successor is { } successor) successor.Post(d, state);
        else _queue.Enqueue(new Work(d, state));
    }

    /// <summary>
    /// <c>Send</c> blocks, and nothing can complete a host future during a
    /// slice; a sync-over-async guest would spin to its CPU
    /// budget, so it fails here instead.
    /// </summary>
    public override void Send(SendOrPostCallback d, object? state) => throw new NotSupportedException();

    public override SynchronizationContext CreateCopy() => this;

    internal bool HasWork => _queue.Count > 0;

    /// <summary>
    /// Moves this context's queued continuations, and every one posted to it
    /// from now on, to <paramref name="successor"/>. Work a dispatch leaves
    /// after its response still resumes on the context it captured, and no
    /// slice drains that context once its dispatch has ended.
    /// </summary>
    internal void HandOver(IsoplaneSynchronizationContext successor)
    {
        _successor = successor;
        while (_queue.TryDequeue(out Work work)) successor.Post(work.Callback, work.State);
    }

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
