using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// The host-driven event loop: timers park the guest on host-owned wake
/// sources, ready thread-pool work runs before parking, continuations resume
/// on the next wake, and registrations are retained once and released when
/// their owner is done or cancelled.
/// </summary>
internal sealed class EventLoop
{
    private int _abandonedCallbacks;
    private int _requests;
    private Timer? _inactiveTimer;

    public async Task<string> RunAsync(string[] path)
    {
        _requests++;
        switch (path[0])
        {
            case "custom-context":
            case "custom-scheduler":
            case "custom-awaiter":
            case "async-void":
                return await Scheduling.RunAsync(path[0]);
            case "delay":
                await Task.Delay(Milliseconds(path[1])).ConfigureAwait(false);
                return "resumed";
            case "delays":
            {
                int count = int.Parse(path[1], CultureInfo.InvariantCulture);
                var delays = Enumerable.Range(1, count).Select(index => Task.Delay(10 * index)).ToArray();
                await Task.WhenAll(delays).ConfigureAwait(false);
                return count.ToString(CultureInfo.InvariantCulture);
            }
            case "sequence":
            {
                // Each await resumes on a distinct host wake before the next timer exists.
                int count = int.Parse(path[1], CultureInfo.InvariantCulture);
                for (int index = 0; index < count; index++)
                {
                    await Task.Delay(10).ConfigureAwait(false);
                }
                return count.ToString(CultureInfo.InvariantCulture);
            }
            case "cancel":
            {
                using var cancellation = new CancellationTokenSource();
                cancellation.CancelAfter(Milliseconds(path[1]));
                try
                {
                    await Task.Delay(10_000, cancellation.Token).ConfigureAwait(false);
                    return "elapsed";
                }
                catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
                {
                    return "cancelled";
                }
            }
            case "cancel-then-delay":
            {
                using var cancellation = new CancellationTokenSource();
                Task cancelled = Task.Delay(100, cancellation.Token);
                cancellation.Cancel();
                await cancelled.ConfigureAwait(ConfigureAwaitOptions.SuppressThrowing);
                if (!cancelled.IsCanceled) throw new InvalidOperationException("the first delay was not cancelled");
                await Task.Delay(Milliseconds(path[1])).ConfigureAwait(false);
                return "resumed";
            }
            case "dispose-then-delay":
            {
                var timer = new Timer(_ => _abandonedCallbacks++, null, 100, Timeout.Infinite);
                timer.Dispose();
                await Task.Delay(Milliseconds(path[1])).ConfigureAwait(false);
                return "resumed";
            }
            case "cancel-only":
            {
                using var cancellation = new CancellationTokenSource();
                Task cancelled = Task.Delay(100, cancellation.Token);
                cancellation.Cancel();
                await cancelled.ConfigureAwait(ConfigureAwaitOptions.SuppressThrowing);
                if (!cancelled.IsCanceled) throw new InvalidOperationException("the first delay was not cancelled");
                return "cancelled";
            }
            case "ready-work":
            {
                // Queued work must run in this slice, before the timer parks the guest.
                int ran = 0;
                var done = new TaskCompletionSource();
                ThreadPool.UnsafeQueueUserWorkItem(_ => { ran++; done.SetResult(); }, null);
                await done.Task.ConfigureAwait(false);
                await Task.Delay(10).ConfigureAwait(false);
                return "ran " + ran.ToString(CultureInfo.InvariantCulture);
            }
            case "yield":
            {
                // A yield is ready work, not a wake source: it completes without parking.
                await Task.Yield();
                return "yielded";
            }
            case "abandon":
                _inactiveTimer = new Timer(_ => _abandonedCallbacks++, null, Timeout.Infinite, Timeout.Infinite);
                _ = AbandonedDelay();
                return "armed";
            case "rearm":
                (_inactiveTimer ?? throw new InvalidOperationException("arm the retained timer first")).Change(10, Timeout.Infinite);
                return _requests.ToString(CultureInfo.InvariantCulture) + ":" + _abandonedCallbacks.ToString(CultureInfo.InvariantCulture);
            case "observe":
                return _requests.ToString(CultureInfo.InvariantCulture) + ":" + _abandonedCallbacks.ToString(CultureInfo.InvariantCulture);
            case "sync-wait":
                // Blocking on a task that needs a wake cannot be honoured without threads.
                return Attempt(() => Task.Delay(10).Wait(TimeSpan.FromSeconds(1)), "waited");
            case "sync-result":
                return Attempt(() => _ = ValueAfterDelay().Result, "resulted");
            case "wait-all":
                return Attempt(() => Task.WaitAll(Task.Delay(10), Task.Delay(20)), "waited");
            case "wait-any":
                return Attempt(() => Task.WaitAny(Task.Delay(10), Task.Delay(20)), "waited");
            case "thread-sleep":
                return Attempt(() => Thread.Sleep(10), "slept");
            case "semaphore-wait":
                return Attempt(() => new SemaphoreSlim(0).Wait(100), "waited");
            case "reset-event-wait":
                return Attempt(() => new ManualResetEventSlim(false).Wait(100), "waited");
            case "thread-start":
                return Attempt(() => new Thread(() => { }).Start(), "started");
            case "blocking-enumerable":
                return Attempt(() =>
                {
                    foreach (int _ in Values().ToBlockingEnumerable())
                    {
                    }
                }, "enumerated");
            case "completed-wait":
                // A completed task needs no wake, so waiting on it is not blocking.
                return Attempt(() => Task.CompletedTask.Wait(), "waited");
            default:
                throw new ArgumentException("unknown loop case " + path[0]);
        }
    }

    private async Task AbandonedDelay()
    {
        await Task.Delay(100).ConfigureAwait(false);
        _abandonedCallbacks++;
    }

    private static async Task<int> ValueAfterDelay()
    {
        await Task.Delay(10).ConfigureAwait(false);
        return 1;
    }

    private static async IAsyncEnumerable<int> Values()
    {
        await Task.Delay(10).ConfigureAwait(false);
        yield return 1;
    }

    /// <summary>A blocking call either completes or names its exception; a trap is the failure.</summary>
    private static string Attempt(Action operation, string completed)
    {
        try
        {
            operation();
            return completed;
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    private static int Milliseconds(string text) => int.Parse(text, CultureInfo.InvariantCulture);
}
