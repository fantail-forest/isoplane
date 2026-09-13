using System;
using System.Collections.Generic;
using System.Runtime.CompilerServices;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

internal static class Scheduling
{
    internal static async Task<string> RunAsync(string scenario)
    {
        switch (scenario)
        {
            case "custom-context":
                var previous = SynchronizationContext.Current;
                var context = new ForwardingContext();
                SynchronizationContext.SetSynchronizationContext(context);
                try
                {
                    await Task.Delay(50);
                    if (context.Posts == 0) throw new InvalidOperationException("custom context did not receive its continuation");
                    return "custom-context resumed";
                }
                finally { SynchronizationContext.SetSynchronizationContext(previous); }
            case "custom-scheduler":
                var scheduler = new ForwardingScheduler();
                await Task.Factory.StartNew(async () =>
                {
                    if (TaskScheduler.Current != scheduler) throw new InvalidOperationException("custom scheduler was not selected");
                    await Task.Delay(50);
                    if (TaskScheduler.Current != scheduler) throw new InvalidOperationException("custom scheduler lost its continuation");
                }, CancellationToken.None, TaskCreationOptions.None, scheduler).Unwrap();
                return "custom-scheduler resumed";
            case "custom-awaiter":
                await new DelayedAwaitable();
                return "custom-awaiter resumed";
            case "async-void":
                var completion = new TaskCompletionSource<string>();
                CompleteAsync(completion);
                return await completion.Task;
            default: throw new ArgumentException("unknown scheduling scenario " + scenario);
        }
    }

    private static async void CompleteAsync(TaskCompletionSource<string> completion)
    {
        await Task.Delay(50);
        completion.SetResult("async-void resumed");
    }

    private sealed class ForwardingContext : SynchronizationContext
    {
        internal int Posts;
        public override void Post(SendOrPostCallback callback, object? state)
            => ThreadPool.QueueUserWorkItem(_ => { Posts++; callback(state); });
    }

    private sealed class ForwardingScheduler : TaskScheduler
    {
        protected override void QueueTask(Task task) => ThreadPool.QueueUserWorkItem(_ => TryExecuteTask(task));
        protected override bool TryExecuteTaskInline(Task task, bool wasQueued) => false;
        protected override IEnumerable<Task> GetScheduledTasks() => Array.Empty<Task>();
    }

    private readonly struct DelayedAwaitable
    {
        public Awaiter GetAwaiter() => new(Task.Delay(50).GetAwaiter());
        internal readonly struct Awaiter(TaskAwaiter inner) : INotifyCompletion
        {
            public bool IsCompleted => inner.IsCompleted;
            public void OnCompleted(Action continuation) => inner.OnCompleted(continuation);
            public void GetResult() => inner.GetResult();
        }
    }
}
