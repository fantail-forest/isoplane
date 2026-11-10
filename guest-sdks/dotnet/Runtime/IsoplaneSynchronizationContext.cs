using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;

namespace Isoplane.Runtime;

/// <summary>
/// Queues continuations posted through Isoplane's synchronization context in
/// guest memory. Each dispatch has its own, so a continuation runs in a slice
/// of the dispatch that awaited; <see cref="Instance"/> serves code outside any
/// dispatch, and takes over a dispatch's continuations once it ends. The export
/// driver runs a bounded batch from the current dispatch's queue per slice, and
/// stops the batch at a continuation <see cref="Task.Yield"/> posted in the
/// same slice, so a yield returns control to the host. This scheduler does not
/// intercept awaiters that bypass context capture or prove that an incomplete
/// task has a completion source.
/// </summary>
internal sealed class IsoplaneSynchronizationContext : SynchronizationContext
{
    internal static readonly IsoplaneSynchronizationContext Instance = new();

    private static readonly YieldCallbacks Yields = YieldCallbacks.Observe();
    private static long _slice = 1;

    private readonly Queue<Work> _queue = new();
    private IsoplaneSynchronizationContext? _successor;

    private readonly struct Work
    {
        internal readonly SendOrPostCallback Callback;
        internal readonly object? State;
        /// <summary>The slice in which <see cref="Task.Yield"/> posted this work; 0 for other work.</summary>
        internal readonly long YieldedInSlice;
        internal Work(SendOrPostCallback cb, object? state, long yieldedInSlice)
        {
            Callback = cb;
            State = state;
            YieldedInSlice = yieldedInSlice;
        }
    }

    /// <summary>
    /// Marks the start of a slice, or of a drain pass that has no host to
    /// return to. Yields posted before this point become runnable.
    /// </summary>
    internal static void BeginSlice() => _slice++;

    public override void Post(SendOrPostCallback d, object? state) =>
        Enqueue(new Work(d, state, Yields.Posted(d) ? _slice : 0));

    private void Enqueue(Work work)
    {
        if (_successor is { } successor) successor.Enqueue(work);
        else _queue.Enqueue(work);
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
        while (_queue.TryDequeue(out Work work)) successor.Enqueue(work);
    }

    /// <summary>
    /// Runs at most one callback batch, preserving the remaining queue for
    /// another slice. The batch ends early at a yield posted in this slice;
    /// the work queued behind it waits too, so the queue's order holds.
    /// </summary>
    internal void Drain()
    {
        for (int remaining = 64; remaining > 0 && _queue.Count > 0; remaining--)
        {
            if (_queue.Peek().YieldedInSlice == _slice) return;
            Work w = _queue.Dequeue();
            w.Callback(w.State);
        }
    }

    /// <summary>
    /// The callbacks <see cref="Task.Yield"/> posts to a synchronization
    /// context. The runtime exposes no way to tell a yield from any other
    /// post, and it posts a yield through one of two callbacks it allocates
    /// once, so they are learned by yielding under an observing context.
    /// </summary>
    private readonly struct YieldCallbacks
    {
        private readonly SendOrPostCallback? _fromAsyncMethod;
        private readonly SendOrPostCallback? _fromAwaiter;

        private YieldCallbacks(SendOrPostCallback? fromAsyncMethod, SendOrPostCallback? fromAwaiter)
        {
            _fromAsyncMethod = fromAsyncMethod;
            _fromAwaiter = fromAwaiter;
        }

        internal bool Posted(SendOrPostCallback callback) =>
            ReferenceEquals(callback, _fromAsyncMethod) || ReferenceEquals(callback, _fromAwaiter);

        internal static YieldCallbacks Observe()
        {
            SynchronizationContext? previous = SynchronizationContext.Current;
            var observer = new Observer();
            SynchronizationContext.SetSynchronizationContext(observer);
            try
            {
                _ = YieldOnceAsync();
                SendOrPostCallback? fromAsyncMethod = observer.Callback;
                // Resumes the method above so that it completes.
                fromAsyncMethod?.Invoke(observer.State);

                observer.Callback = null;
                Task.Yield().GetAwaiter().OnCompleted(static () => { });
                return new YieldCallbacks(fromAsyncMethod, observer.Callback);
            }
            finally
            {
                SynchronizationContext.SetSynchronizationContext(previous);
            }
        }

        private static async Task YieldOnceAsync() => await Task.Yield();

        private sealed class Observer : SynchronizationContext
        {
            internal SendOrPostCallback? Callback;
            internal object? State;

            public override void Post(SendOrPostCallback d, object? state)
            {
                Callback = d;
                State = state;
            }
        }
    }
}
