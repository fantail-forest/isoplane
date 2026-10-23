using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Text;
using Xunit;

namespace Isoplane.Analyzers.Tests;

public sealed class AwaitFixTests
{
    [Theory]
    [InlineData("public class Worker { public void Pause() { Thread.Sleep(10); } }")]
    [InlineData("public abstract class Base { public abstract void Pause(); } public class Worker : Base { public override void Pause() { Thread.Sleep(10); } }")]
    [InlineData("public class Worker : IDisposable { public void Dispose() { Thread.Sleep(10); } }")]
    [InlineData("public class Worker : IDisposable { void IDisposable.Dispose() { Thread.Sleep(10); } }")]
    [InlineData("public class Worker { public void Pause() { Thread.Sleep(10); } public Action Callback => Pause; }")]
    [InlineData("public delegate void Callback(); public class Worker { public void Pause() { Thread.Sleep(10); } public Callback Handler => Pause; }")]
    [InlineData("public class Worker { public event Action? Started; public Worker() { Started += Pause; } public void Pause() { Thread.Sleep(10); } }")]
    [InlineData("public class Worker { public void Pause() { Thread.Sleep(10); } public void Work() { Pause(); } }")]
    [InlineData("public class Worker { public void Pause() { Thread.Sleep(10); } public async Task WorkAsync() { Pause(); await Task.Yield(); } }")]
    public async Task a_synchronous_method_keeps_its_return_type_and_callers(string declarations)
    {
        var source = "using System; using System.Threading; using System.Threading.Tasks;\n" + declarations;
        Assert.Equal(["ISOPLANE1003"], await Application.AnalyzeAsync(source));
        Assert.Null(await Application.FixAsync(source, "ISOPLANE1003"));
    }

    [Theory]
    [InlineData("Func<Task> work = async () => { Thread.Sleep(10); }; await work();")]
    [InlineData("Func<Task> work = async delegate { Thread.Sleep(10); }; await work();")]
    [InlineData("async Task PauseAsync() { Thread.Sleep(10); } await PauseAsync();")]
    public async Task an_existing_asynchronous_function_can_await_without_changing_its_contract(string statements)
    {
        var source = Application.Library("public static async Task WorkAsync() { await Task.Yield(); " + statements + " }")
            .Replace("using System;", "using System;\nusing System.Threading;");
        Assert.Equal(["ISOPLANE1003"], await Application.AnalyzeAsync(source));
        var changed = await Application.FixAsync(source, "ISOPLANE1003");
        Assert.Equal(source.Replace("Thread.Sleep(10)", "await Task.Delay(10)"), changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!));
    }

    [Theory]
    [InlineData("public static void Call() { Program.Main([]); }")]
    [InlineData("public static Action<string[]> Callback => Program.Main;")]
    public async Task a_synchronous_entry_point_referenced_from_another_file_keeps_its_contract(string member)
    {
        const string source = "using System.Threading; public static class Program { public static void Main(string[] args) { Thread.Sleep(10); } }";
        var project = Application.Project(source, OutputKind.ConsoleApplication)
            .AddDocument("Caller.cs", SourceText.From("using System; public static class Caller { " + member + " }")).Project;
        Assert.Equal(["ISOPLANE1003"], (await Application.DiagnosticsAsync(project)).Select(diagnostic => diagnostic.Id));
        Assert.Null(await Application.FixAsync(project.Documents.First(), "ISOPLANE1003"));
    }

    [Fact]
    public async Task an_await_inside_an_unsafe_block_is_not_offered()
    {
        var source = Application.Library("public static async Task WorkAsync() { await Task.Yield(); unsafe { System.Threading.Thread.Sleep(10); } }");
        var project = Application.Project(source);
        project = project.WithCompilationOptions(((CSharpCompilationOptions)project.CompilationOptions!).WithAllowUnsafe(true));
        Assert.Equal(["ISOPLANE1003"], (await Application.DiagnosticsAsync(project)).Select(diagnostic => diagnostic.Id));
        Assert.Null(await Application.FixAsync(project.Documents.Single(), "ISOPLANE1003"));
    }

    [Fact]
    public async Task a_method_named_main_in_a_library_is_not_treated_as_the_entry_point()
    {
        const string source = "using System.Threading; public static class Program { public static void Main() { Thread.Sleep(10); } }";
        Assert.Equal(["ISOPLANE1003"], await Application.AnalyzeAsync(source));
        Assert.Null(await Application.FixAsync(source, "ISOPLANE1003"));
    }
}
