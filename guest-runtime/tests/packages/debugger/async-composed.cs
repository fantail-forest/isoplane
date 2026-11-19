using System;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;

namespace Debugger.Composed;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/async-composed", Handle);

    private static async Task<string> Handle()
    {
        int seed = 21;
        int result = await Outer(seed);
        return (result + seed - 21).ToString();
    }

    private static async Task<int> Outer(int input)
    {
        int outer = input + 1;
        var outerSample = new DebugSample { Value = 1 };
        int[] outerNumbers = [2, 3];
        Task<int> child = Inner(outer, outerSample, outerNumbers);
        Task<int[]> combined = Task.WhenAll(child, Task.FromResult(0));
        Task<Task<int[]>> choice = Task.WhenAny(combined, new TaskCompletionSource<int[]>().Task);
        Task<int[]> winner = await choice;
        int result = (await winner.ConfigureAwait(false))[0];
        EnsureTrackingReleased(child, combined, choice);
        outer += 1;
        return result + outer + outerSample.Value + outerNumbers[1];
    }

    private static async Task<int> Inner(int input, DebugSample watched, int[] watchedNumbers)
    {
        int answer = input;
        var sample = new DebugSample { Value = 7, Name = "hello" };
        int[] numbers = [3, 4];
        await new DebugYield();
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

    private readonly struct DebugYield : System.Runtime.CompilerServices.ICriticalNotifyCompletion
    {
        public DebugYield GetAwaiter() => this;
        public bool IsCompleted => false;
        public void GetResult() { }
        public void OnCompleted(Action continuation) => Task.Yield().GetAwaiter().OnCompleted(continuation);
        public void UnsafeOnCompleted(Action continuation) => Task.Yield().GetAwaiter().UnsafeOnCompleted(continuation);
    }
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
