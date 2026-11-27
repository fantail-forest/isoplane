using System;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;

namespace Debugger.Pooled;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/async-pooled", Handle);

    private static async Task<string> Handle()
    {
        int seed = 21;
        int result = await Outer(seed);
        await VerifyPoolReuse();
        return (result + seed - 21).ToString();
    }

    [System.Runtime.CompilerServices.AsyncMethodBuilder(typeof(System.Runtime.CompilerServices.PoolingAsyncValueTaskMethodBuilder<>))]
    private static async ValueTask<int> Outer(int input)
    {
        int outer = input + 1;
        var outerSample = new DebugSample { Value = 1 };
        int[] outerNumbers = [2, 3];
        int result = await Inner(outer, outerSample, outerNumbers).ConfigureAwait(false);
        outer += 1;
        return result + outer + outerSample.Value + outerNumbers[1];
    }

    [System.Runtime.CompilerServices.AsyncMethodBuilder(typeof(System.Runtime.CompilerServices.PoolingAsyncValueTaskMethodBuilder<>))]
    private static async ValueTask<int> Inner(int input, DebugSample watched, int[] watchedNumbers)
    {
        int answer = input;
        var sample = new DebugSample { Value = 7, Name = "hello" };
        int[] numbers = [3, 4];
        Task delay = Task.Delay(100);
        if (delay.IsCompleted) throw new InvalidOperationException("the host timer must be pending before await");
        await delay;
        int total = ComputeTotal(answer, sample, numbers);
        await Task.Delay(1);
        GC.Collect();
        watched.Value += 1;
        watchedNumbers[1] += 1;
        return total;
    }

    private static int ComputeTotal(int answer, DebugSample sample, int[] numbers)
    {
        int sum = answer + sample.Value + numbers[1];
        return sum;
    }

    private sealed class DebugSample
    {
        public int Value;
        public string Name = "";
    }
    private static async Task VerifyPoolReuse()
    {
        ValueTask<int> first = ReusedOperation(false);
        object box = OperationBox(first);
        if (await first != 7) throw new InvalidOperationException("The pooled operation must return its result.");
        EnsureTrackingReleased(box);

        ValueTask<int> failed = ReusedOperation(true);
        if (!ReferenceEquals(box, OperationBox(failed))) throw new InvalidOperationException("The consumed operation must return its box to the pool.");
        try
        {
            await failed;
            throw new InvalidOperationException("The pooled operation must propagate its failure.");
        }
        catch (ArgumentException error) when (error.Message == "pooled failure") { }
        EnsureTrackingReleased(box);

        ValueTask<int> reused = ReusedOperation(false);
        if (!ReferenceEquals(box, OperationBox(reused))) throw new InvalidOperationException("A failed operation must return its box to the pool.");
        if (await reused != 7) throw new InvalidOperationException("A reused operation must retain its result.");
        EnsureTrackingReleased(box);
    }

    [System.Runtime.CompilerServices.AsyncMethodBuilder(typeof(System.Runtime.CompilerServices.PoolingAsyncValueTaskMethodBuilder<>))]
    private static async ValueTask<int> ReusedOperation(bool fail)
    {
        await Task.Yield();
        if (fail) throw new ArgumentException("pooled failure");
        return 7;
    }

    private static object OperationBox(ValueTask<int> operation) =>
        (typeof(ValueTask<int>).GetField("_obj", System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)
            ?? throw new InvalidOperationException("The value task backing field is missing.")).GetValue(operation)
            ?? throw new InvalidOperationException("An incomplete pooled operation must have a backing box.");
    private static void EnsureTrackingReleased(params object[] completed)
    {
        var field = typeof(Task).GetField("s_isoplaneAsyncState", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Static)
            ?? throw new InvalidOperationException("Async debugger tracking is missing.");
        var tracking = field.GetValue(null) ?? throw new InvalidOperationException("Async debugger tracking is disabled.");
        foreach (string list in new[] { "Frames", "Dependencies" })
        {
            for (object? node = TrackingField(tracking, list); node is not null; node = TrackingField(node, "Next"))
            {
                foreach (object operation in completed)
                    if (ReferenceEquals(TrackingField(node, "Operation"), operation)
                        || (list == "Dependencies" && ReferenceEquals(TrackingField(node, "Awaited"), operation)))
                        throw new InvalidOperationException("Completed operations must leave async debugger tracking.");
            }
        }
    }

    [System.Diagnostics.CodeAnalysis.UnconditionalSuppressMessage("Trimming", "IL2075",
        Justification = "The debug runtime accesses these async tracking fields directly and retains them.")]
    private static object? TrackingField(object value, string name) =>
        (value.GetType().GetField(name, System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)
            ?? throw new InvalidOperationException("Missing async tracking field: " + name)).GetValue(value);
}
