using Isoplane.Build;
using Microsoft.CodeAnalysis;
using Xunit;

namespace Isoplane.Analyzers.Tests;

public sealed class BlockingCallTests
{
    /// <summary>
    /// A class whose one asynchronous method has <paramref name="statements"/> as its body, beside a type
    /// whose <c>Result</c> and <c>GetResult</c> are not a task's.
    /// </summary>
    private static string Method(string statements, string signature = "public static async Task WorkAsync(Task task, Task<int> number, ValueTask<int> count, SemaphoreSlim gate, CancellationToken cancellation)") => $$"""
        using System;
        using System.Threading;
        using System.Threading.Tasks;

        public sealed class Outcome
        {
            public int Result => 1;
            public int GetResult() => 1;
        }

        public static class Worker
        {
            {{signature}}
            {
                await Task.Yield();
                {{statements}}
            }
        }

        """.ReplaceLineEndings("\n");

    /// <summary>Statements, and the diagnostics the analyzer and the build's check of the assembly must both draw.</summary>
    public static TheoryData<string, string[]> Cases => new()
    {
        { "await Task.Delay(1, cancellation);", [] },
        { "await gate.WaitAsync(cancellation);", [] },
        { "_ = task.IsCompleted;", [] },
        { "_ = new Outcome().Result;", [] },
        { "_ = new Outcome().GetResult();", [] },
        { "Task.Yield().GetAwaiter().GetResult();", [] },
        { "new Thread(() => { }).Start();", ["ISOPLANE1002"] },
        { "Thread.Sleep(10);", ["ISOPLANE1003"] },
        { "Thread.CurrentThread.Join();", ["ISOPLANE1003"] },
        { "task.Wait();", ["ISOPLANE1003"] },
        { "task.Wait(cancellation);", ["ISOPLANE1003"] },
        { "Task.WaitAll(task, task);", ["ISOPLANE1003"] },
        { "Task.WaitAny(task, task);", ["ISOPLANE1003"] },
        { "_ = number.Result;", ["ISOPLANE1003"] },
        { "_ = count.Result;", ["ISOPLANE1003"] },
        { "_ = number.Result.ToString();", ["ISOPLANE1003"] },
        { "gate.Wait();", ["ISOPLANE1003"] },
        { "lock (gate) Monitor.Wait(gate);", ["ISOPLANE1003"] },
        { "Action<int> sleep = Thread.Sleep;\n        sleep(1);", ["ISOPLANE1003"] },
        { "Thread.Sleep(10);\n        task.Wait();", ["ISOPLANE1003", "ISOPLANE1003"] },
    };

    [Theory]
    [MemberData(nameof(Cases))]
    public async Task each_blocking_call_is_reported_where_it_is_written(string statements, string[] expected)
    {
        Assert.Equal(expected, await Application.AnalyzeAsync(Method(statements)));
    }

    [Theory]
    [MemberData(nameof(Cases))]
    public async Task the_analyzer_and_the_build_check_of_the_assembly_report_the_same_rules(string statements, string[] expected)
    {
        Assert.Equal(expected.Distinct().Order(), await InspectAsync(statements));
    }

    /// <summary>
    /// Calls of an awaiter's <c>GetResult</c>, which only the analyzer reports: every <c>await</c> compiles
    /// to the same call, so the build's check of the assembly cannot tell a written one from the compiler's.
    /// </summary>
    public static TheoryData<string> AwaiterCases =>
    [
        "task.GetAwaiter().GetResult();",
        "_ = number.GetAwaiter().GetResult();",
        "default(ValueTask).GetAwaiter().GetResult();",
        "_ = count.GetAwaiter().GetResult();",
        "task.ConfigureAwait(false).GetAwaiter().GetResult();",
        "_ = number.ConfigureAwait(false).GetAwaiter().GetResult();",
        "default(ValueTask).ConfigureAwait(false).GetAwaiter().GetResult();",
        "_ = count.ConfigureAwait(false).GetAwaiter().GetResult();",
        "Action wait = task.GetAwaiter().GetResult;\n        wait();",
    ];

    [Theory]
    [MemberData(nameof(AwaiterCases))]
    public async Task a_written_call_of_an_awaiter_get_result_is_reported_where_it_is_written(string statements)
    {
        Assert.Equal(["ISOPLANE1003"], await Application.AnalyzeAsync(Method(statements)));
    }

    [Theory]
    [MemberData(nameof(AwaiterCases))]
    public async Task the_build_check_of_the_assembly_leaves_an_awaiter_get_result_to_the_analyzer(string statements)
    {
        Assert.Empty(await InspectAsync(statements));
    }

    /// <summary>The rules the build's check of the compiled assembly reports for the method.</summary>
    private static async Task<IEnumerable<string>> InspectAsync(string statements)
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-blocking-agreement-");
        try
        {
            var assembly = Path.Combine(directory.FullName, "App.dll");
            var emitted = (await Application.Project(Method(statements)).GetCompilationAsync())!.Emit(assembly);
            Assert.True(emitted.Success, string.Join("; ", emitted.Diagnostics));
            return AssemblyClosure.Diagnostics([AssemblyClosure.InspectFile(assembly)]).Select(diagnostic => diagnostic.Code).Distinct().Order().ToArray();
        }
        finally { directory.Delete(recursive: true); }
    }

    [Theory]
    [InlineData("Thread.Sleep(10);", "await Task.Delay(10);")]
    [InlineData("task.Wait();", "await task;")]
    [InlineData("Task.WaitAll(task, task);", "await Task.WhenAll(task, task);")]
    [InlineData("Task.WaitAny(task, task);", "await Task.WhenAny(task, task);")]
    [InlineData("gate.Wait();", "await gate.WaitAsync();")]
    [InlineData("gate.Wait(cancellation);", "await gate.WaitAsync(cancellation);")]
    [InlineData("_ = number.Result;", "_ = await number;")]
    [InlineData("_ = count.Result;", "_ = await count;")]
    [InlineData("_ = number.Result + 1;", "_ = await number + 1;")]
    [InlineData("_ = number.Result.ToString();", "_ = (await number).ToString();")]
    [InlineData("task.GetAwaiter().GetResult();", "await task;")]
    [InlineData("_ = number.GetAwaiter().GetResult();", "_ = await number;")]
    [InlineData("_ = count.GetAwaiter().GetResult();", "_ = await count;")]
    [InlineData("task.ConfigureAwait(false).GetAwaiter().GetResult();", "await task.ConfigureAwait(false);")]
    [InlineData("_ = number.ConfigureAwait(false).GetAwaiter().GetResult();", "_ = await number.ConfigureAwait(false);")]
    [InlineData("_ = count.ConfigureAwait(false).GetAwaiter().GetResult().ToString();", "_ = (await count.ConfigureAwait(false)).ToString();")]
    public async Task a_wait_with_an_asynchronous_counterpart_is_replaced_by_awaiting_it(string blocking, string awaiting)
    {
        var changed = await Application.FixAsync(Method(blocking), "ISOPLANE1003");
        Assert.Equal(Method(awaiting), changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!));
    }

    [Fact]
    public async Task an_unreferenced_synchronous_entry_point_can_become_asynchronous()
    {
        const string before = "using System.Threading;\n\npublic static class Worker\n{\n    public static void Main()\n    {\n        Thread.Sleep(10);\n    }\n}\n";
        Assert.Equal(["ISOPLANE1003"], await Application.AnalyzeAsync(before, OutputKind.ConsoleApplication));
        var changed = await Application.FixAsync(before, "ISOPLANE1003", OutputKind.ConsoleApplication);
        Assert.Equal("using System.Threading;\nusing System.Threading.Tasks;\n\npublic static class Worker\n{\n    public static async Task Main()\n    {\n        await Task.Delay(10);\n    }\n}\n", changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!, OutputKind.ConsoleApplication));
    }

    /// <summary>Waits with no asynchronous counterpart of the same meaning, and code where an await cannot simply be written.</summary>
    [Theory]
    [InlineData("task.Wait(TimeSpan.FromSeconds(1));")]
    [InlineData("int first = Task.WaitAny(task, task);")]
    [InlineData("Thread.CurrentThread.Join();")]
    [InlineData("var awaiter = task.GetAwaiter();\n        awaiter.GetResult();")]
    [InlineData("Action wait = () => task.Wait();\n        wait();")]
    [InlineData("lock (gate) task.Wait();")]
    public async Task a_wait_without_one_clear_repair_offers_none(string statements)
    {
        Assert.Null(await Application.FixAsync(Method(statements), "ISOPLANE1003"));
    }

    [Fact]
    public async Task a_method_that_returns_a_value_is_reported_without_a_fix()
    {
        Assert.Null(await Application.FixAsync(Method("Thread.Sleep(10);\n        return 1;", "public static int Work(Task task)").Replace("        await Task.Yield();\n", ""), "ISOPLANE1003"));
    }
}
