using Isoplane.Build;
using Microsoft.CodeAnalysis;
using Xunit;

namespace Isoplane.Analyzers.Tests;

public sealed class BlockingCallTests
{
    /// <summary>A class whose one asynchronous method has <paramref name="statements"/> as its body.</summary>
    private static string Method(string statements, string signature = "public static async Task WorkAsync(Task task, SemaphoreSlim gate, CancellationToken cancellation)") => $$"""
        using System;
        using System.Threading;
        using System.Threading.Tasks;

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
        { "new Thread(() => { }).Start();", ["ISOPLANE1002"] },
        { "Thread.Sleep(10);", ["ISOPLANE1003"] },
        { "Thread.CurrentThread.Join();", ["ISOPLANE1003"] },
        { "task.Wait();", ["ISOPLANE1003"] },
        { "task.Wait(cancellation);", ["ISOPLANE1003"] },
        { "Task.WaitAll(task, task);", ["ISOPLANE1003"] },
        { "Task.WaitAny(task, task);", ["ISOPLANE1003"] },
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
        var directory = Directory.CreateTempSubdirectory("isoplane-blocking-agreement-");
        try
        {
            var assembly = Path.Combine(directory.FullName, "App.dll");
            var emitted = (await Application.Project(Method(statements)).GetCompilationAsync())!.Emit(assembly);
            Assert.True(emitted.Success, string.Join("; ", emitted.Diagnostics));
            var fromAssembly = AssemblyClosure.Diagnostics([AssemblyClosure.InspectFile(assembly)]).Select(diagnostic => diagnostic.Code).Distinct().Order();
            Assert.Equal(expected.Distinct().Order(), fromAssembly);
        }
        finally { directory.Delete(recursive: true); }
    }

    [Theory]
    [InlineData("Thread.Sleep(10);", "await Task.Delay(10);")]
    [InlineData("task.Wait();", "await task;")]
    [InlineData("Task.WaitAll(task, task);", "await Task.WhenAll(task, task);")]
    [InlineData("gate.Wait();", "await gate.WaitAsync();")]
    [InlineData("gate.Wait(cancellation);", "await gate.WaitAsync(cancellation);")]
    public async Task a_wait_with_an_asynchronous_counterpart_is_replaced_by_awaiting_it(string blocking, string awaiting)
    {
        var changed = await Application.FixAsync(Method(blocking), "ISOPLANE1003");
        Assert.Equal(Method(awaiting), changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!));
    }

    [Fact]
    public async Task the_fix_makes_a_method_that_returns_nothing_asynchronous()
    {
        const string before = "using System.Threading;\n\npublic static class Worker\n{\n    public static void Pause()\n    {\n        Thread.Sleep(10);\n    }\n}\n";
        var changed = await Application.FixAsync(before, "ISOPLANE1003");
        Assert.Equal("using System.Threading;\nusing System.Threading.Tasks;\n\npublic static class Worker\n{\n    public static async Task Pause()\n    {\n        await Task.Delay(10);\n    }\n}\n", changed);
    }

    /// <summary>Waits with no asynchronous counterpart of the same meaning, and code where an await cannot simply be written.</summary>
    [Theory]
    [InlineData("task.Wait(TimeSpan.FromSeconds(1));")]
    [InlineData("Task.WaitAny(task, task);")]
    [InlineData("Thread.CurrentThread.Join();")]
    [InlineData("Action wait = () => task.Wait();\n        wait();")]
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
