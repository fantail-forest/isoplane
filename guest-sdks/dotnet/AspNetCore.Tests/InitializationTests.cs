using Isoplane.Abi;
using Isoplane;
using Isoplane.Runtime;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class InitializationTests
{
    [Fact]
    public async Task Callbacks_are_awaited_in_order_before_the_hook_completes()
    {
        var hook = new LifecycleHook();
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var calls = new List<int>();
        Func<Task> callbacks = async () => { calls.Add(1); await release.Task; calls.Add(2); };
        callbacks += () => { calls.Add(3); return Task.CompletedTask; };
        var pending = hook.EnterAsync(callbacks);
        Assert.False(pending.IsCompleted, "the hook must await an unfinished callback");
        Assert.True(calls.SequenceEqual([1]), "later callbacks must wait for earlier callbacks");
        Assert.True(ReferenceEquals(pending, hook.EnterAsync(callbacks)), "reentering the hook must preserve its original completion");
        release.SetResult();
        await pending;
        Assert.True(hook.IsCompleted && calls.SequenceEqual([1, 2, 3]), "completion must include every callback in registration order");
    }

    [Fact]
    public async Task An_asynchronous_failure_prevents_later_callbacks_and_remains_failed()
    {
        var hook = new LifecycleHook();
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var reached = false;
        Func<Task> callbacks = async () => { await release.Task; throw new InvalidOperationException("warmup failed"); };
        callbacks += () => { reached = true; return Task.CompletedTask; };
        var pending = hook.EnterAsync(callbacks);
        release.SetResult();
        var error = await Record.ExceptionAsync(() => pending);
        Assert.True(error is InvalidOperationException { Message: "warmup failed" },
            "the original callback failure must reach the lifecycle caller");
        Assert.False(reached || hook.IsCompleted, "a failed hook must not proceed or appear successful");
        Assert.True(ReferenceEquals(pending, hook.EnterAsync(callbacks)), "reentering the hook must preserve its original completion");
    }

    [Fact]
    public async Task Failed_user_initialization_prevents_framework_completion()
    {
        var hook = new LifecycleHook();
        bool completed = false;
        var failure = await Record.ExceptionAsync(() => hook.EnterAsync(
            () => throw new InvalidOperationException("user initialization failed"),
            () => { completed = true; return Task.CompletedTask; }));
        Assert.True(failure is InvalidOperationException && !completed && !hook.IsCompleted,
            "framework startup must not proceed when user initialization fails");
    }

    [Fact]
    public async Task A_completed_hook_does_not_run_callbacks_again()
    {
        var hook = new LifecycleHook();
        int calls = 0;
        Func<Task> callback = () => { calls++; return Task.CompletedTask; };
        await hook.EnterAsync(callback);
        await hook.EnterAsync(callback);
        Assert.True(calls == 1, "subsequent entries must retain the completed lifecycle state");
    }

    [Fact]
    public async Task A_cancelled_callback_prevents_later_callbacks_and_remains_cancelled()
    {
        var hook = new LifecycleHook();
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        bool reached = false;
        Func<Task> callbacks = () => release.Task;
        callbacks += () => { reached = true; return Task.CompletedTask; };
        Task pending = hook.EnterAsync(callbacks);
        release.SetCanceled();
        try
        {
            await pending;
            Assert.Fail("cancelling a callback must cancel lifecycle completion");
        }
        catch (OperationCanceledException)
        {
            Assert.True(pending.IsCanceled, "the hook must preserve cancellation as a cancelled task");
        }
        Assert.False(reached || hook.IsCompleted, "a cancelled hook must neither continue nor appear successful");
        Assert.True(ReferenceEquals(pending, hook.EnterAsync(callbacks)), "a cancelled hook must not restart on reentry");
    }

    [Fact]
    public async Task A_synchronous_callback_failure_is_returned_as_failed_completion()
    {
        var hook = new LifecycleHook();
        var failure = new InvalidOperationException("activation failed");
        Task completion = hook.EnterAsync(() => throw failure);
        try
        {
            await completion;
            Assert.Fail("a synchronous lifecycle exception must fail the returned task");
        }
        catch (InvalidOperationException error)
        {
            Assert.True(ReferenceEquals(error, failure), "lifecycle completion must preserve the original failure");
        }
        Assert.False(hook.IsCompleted, "a failed lifecycle callback must not mark activation successful");
    }

    [Fact]
    public async Task Dispatch_waits_for_activation_and_subsequent_dispatches_do_not_activate_again()
    {
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        int activations = 0;
        Func<Task> callback = async () => { activations++; await release.Task; };
        Activation.Activated += callback;
        try
        {
            var router = new RecordingRouter();
            var record = new DispatchRecord { Kind = DispatchKind.CronTick };
            var context = new Ctx(0, 1);
            Task<Response> dispatch = RequestDispatch.RouteAsync(router, record, context);
            Assert.False(dispatch.IsCompleted || router.Called || Activation.IsActivated,
                "an unfinished activation must prevent dispatch and remain inactive");
            release.SetResult();
            await dispatch;
            Assert.True(router.Called && Activation.IsActivated,
                "dispatch must run once every activation callback has completed");
            await RequestDispatch.RouteAsync(router, record, context);
            Assert.True(activations == 1, "a later dispatch must not repeat activation");
        }
        finally
        {
            Activation.Activated -= callback;
        }
    }

    private sealed class RecordingRouter : IIsoplaneRouter
    {
        internal bool Called { get; private set; }
        public void Boot() { }
        public Task<Response> Route(DispatchRecord record, Ctx context)
        {
            Called = true;
            return Task.FromResult(Response.Text("ready"));
        }
    }
}
