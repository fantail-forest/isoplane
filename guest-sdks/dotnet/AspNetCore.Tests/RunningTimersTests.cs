using Isoplane.Runtime;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

// The test host runs timers of its own, so each test looks for the line naming the timer it started.
public sealed class RunningTimersTests
{
    private static readonly string Here = typeof(RunningTimersTests).FullName!;

    [Fact]
    public void A_periodic_timer_is_listed_by_its_period_and_the_method_it_calls()
    {
        using var timer = new Timer(Tick, null, TimeSpan.FromMilliseconds(900), TimeSpan.FromMilliseconds(900));
        Assert.Contains($"a timer every 900 ms calling {Here}.Tick", RunningTimers.Describe());
    }

    [Fact]
    public void A_timer_that_fires_once_is_listed_by_when_it_is_due_and_the_method_its_lambda_is_in()
    {
        using var timer = new Timer(_ => { }, null, TimeSpan.FromSeconds(7100), Timeout.InfiniteTimeSpan);
        Assert.Contains(
            $"a timer set for 7100 s calling a lambda in {Here}.{nameof(A_timer_that_fires_once_is_listed_by_when_it_is_due_and_the_method_its_lambda_is_in)}",
            RunningTimers.Describe());
    }

    [Fact]
    public void A_timer_calling_a_local_function_is_listed_by_that_function_and_the_method_it_is_in()
    {
        static void Beat(object? state) { }
        using var timer = new Timer(Beat, null, TimeSpan.FromSeconds(7150), Timeout.InfiniteTimeSpan);
        Assert.Contains(
            $"a timer set for 7150 s calling the local function Beat in {Here}.{nameof(A_timer_calling_a_local_function_is_listed_by_that_function_and_the_method_it_is_in)}",
            RunningTimers.Describe());
    }

    [Fact]
    public async Task A_delay_is_listed_by_the_async_method_awaiting_it()
    {
        using var cancellation = new CancellationTokenSource();
        Task waiting = WaitAsync(TimeSpan.FromSeconds(7200), cancellation.Token);
        Assert.Contains($"a delay of 7200 s awaited by {Here}.WaitAsync", RunningTimers.Describe());
        await CancelAsync(cancellation, waiting);
    }

    [Fact]
    public async Task A_delay_awaited_on_a_synchronization_context_is_listed_by_the_async_method_awaiting_it()
    {
        using var cancellation = new CancellationTokenSource();
        SynchronizationContext? ambient = SynchronizationContext.Current;
        SynchronizationContext.SetSynchronizationContext(new ThreadPoolContext());
        Task waiting;
        try
        {
            waiting = WaitOnContextAsync(TimeSpan.FromSeconds(7300), cancellation.Token);
        }
        finally
        {
            SynchronizationContext.SetSynchronizationContext(ambient);
        }
        Assert.Contains($"a delay of 7300 s awaited by {Here}.WaitOnContextAsync", RunningTimers.Describe());
        await CancelAsync(cancellation, waiting);
    }

    [Fact]
    public async Task A_delay_awaited_by_an_async_lambda_is_listed_by_the_method_the_lambda_is_in()
    {
        using var cancellation = new CancellationTokenSource();
        Func<Task> wait = async () => await Task.Delay(TimeSpan.FromSeconds(7350), cancellation.Token).ConfigureAwait(false);
        Task waiting = wait();
        Assert.Contains(
            $"a delay of 7350 s awaited by a lambda in {Here}.{nameof(A_delay_awaited_by_an_async_lambda_is_listed_by_the_method_the_lambda_is_in)}",
            RunningTimers.Describe());
        await CancelAsync(cancellation, waiting);
    }

    [Fact]
    public async Task A_delay_given_to_a_combinator_is_listed_by_the_async_method_awaiting_the_combinator()
    {
        using var cancellation = new CancellationTokenSource();
        var other = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        Task waiting = WaitForEitherAsync(Task.Delay(TimeSpan.FromSeconds(7400), cancellation.Token), other.Task);
        Assert.Contains($"a delay of 7400 s awaited by {Here}.WaitForEitherAsync", RunningTimers.Describe());
        other.SetResult();
        await waiting;
        cancellation.Cancel();
    }

    [Fact]
    public async Task A_delay_continued_by_a_delegate_is_listed_by_the_method_the_delegate_calls()
    {
        using var cancellation = new CancellationTokenSource();
        Task continued = Task.Delay(TimeSpan.FromSeconds(7500), cancellation.Token)
            .ContinueWith(Continue, CancellationToken.None, TaskContinuationOptions.None, TaskScheduler.Default);
        Assert.Contains($"a delay of 7500 s awaited by {Here}.Continue", RunningTimers.Describe());
        cancellation.Cancel();
        await continued;
    }

    [Fact]
    public void A_delay_nothing_awaits_is_listed_as_such()
    {
        using var cancellation = new CancellationTokenSource();
        _ = Task.Delay(TimeSpan.FromSeconds(7600), cancellation.Token);
        Assert.Contains("a delay of 7600 s that nothing awaits", RunningTimers.Describe());
        cancellation.Cancel();
    }

    [Theory]
    [InlineData("Worker", "App.Service.Worker")]
    [InlineData("<Worker>d__19", "App.Service.Worker")]
    [InlineData("<Start>b__9_0", "a lambda in App.Service.Start")]
    [InlineData("<<Start>b__9_0>d", "a lambda in App.Service.Start")]
    [InlineData("<Start>g__Poll|9_1", "the local function Poll in App.Service.Start")]
    [InlineData("<<Start>g__Poll|9_1>d", "the local function Poll in App.Service.Start")]
    [InlineData(".ctor", "the constructor of App.Service")]
    [InlineData("<.ctor>b__4_0", "a lambda in the constructor of App.Service")]
    [InlineData(".cctor", "the static constructor of App.Service")]
    [InlineData("<Main>$", "the program's top-level statements")]
    [InlineData("<<Main>$>d__0", "the program's top-level statements")]
    [InlineData("<<Main>$>b__0_3", "a lambda in the program's top-level statements")]
    [InlineData("<<Main>$>g__Tick|0_1", "the local function Tick in the program's top-level statements")]
    [InlineData("<Clone>$", "App.Service.<Clone>$")]
    [InlineData("<Start>z__1", "App.Service.<Start>z__1")]
    [InlineData("<Start", "App.Service.<Start")]
    public void A_method_is_named_as_written_and_a_generated_name_of_an_unknown_form_is_kept(string name, string expected) =>
        Assert.Equal(expected, SourceNames.Method("App.Service", name));

    [Fact]
    public void A_type_is_named_without_generic_arity_and_a_closure_class_as_the_type_that_wrote_it()
    {
        Func<int> closure = () => Here.Length;
        Assert.Equal(Here, SourceNames.Type(closure.Method.DeclaringType!));
        Assert.Equal("System.Collections.Generic.Dictionary.Enumerator", SourceNames.Type(typeof(Dictionary<int, string>.Enumerator)));
    }

    private static void Tick(object? state) { }

    private static void Continue(Task delay) { }

    private static async Task WaitAsync(TimeSpan delay, CancellationToken cancellation) =>
        await Task.Delay(delay, cancellation).ConfigureAwait(false);

    private static async Task WaitOnContextAsync(TimeSpan delay, CancellationToken cancellation) =>
        await Task.Delay(delay, cancellation);

    private static async Task WaitForEitherAsync(Task delay, Task other) =>
        await Task.WhenAny(delay, other).ConfigureAwait(false);

    private static async Task CancelAsync(CancellationTokenSource cancellation, Task waiting)
    {
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => waiting);
    }

    // Await captures only a context of a derived type; this one resumes on the thread pool as the base does.
    private sealed class ThreadPoolContext : SynchronizationContext;
}
