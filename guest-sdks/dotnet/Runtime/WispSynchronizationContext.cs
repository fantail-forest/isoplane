using System;
using System.Collections.Generic;
using System.Threading;

namespace Wisp.Runtime;

/// <summary>
/// The continuation queue. An <c>await</c> hands its continuation to the
/// current context; the default one would use a thread pool the module does
/// not have, and running it inline would put it on a host frame. Queued
/// here, a continuation is a heap object that runs only on the next call in
/// from the host, so an empty queue with an incomplete root task means the
/// dispatch's whole state is in linear memory.
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
    /// <c>Send</c> blocks, and nothing can complete a host future while the
    /// guest is inside a call; a sync-over-async guest would spin to its CPU
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
