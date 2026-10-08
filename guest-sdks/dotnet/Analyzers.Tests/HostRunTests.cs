using Microsoft.CodeAnalysis;
using Xunit;

namespace Isoplane.Analyzers.Tests;

public sealed class HostRunTests
{
    /// <summary>An ASP.NET program whose last statements are <paramref name="run"/>, as top-level statements.</summary>
    private static string Program(string run) => $"""
        using Microsoft.AspNetCore.Builder;

        var app = WebApplication.CreateBuilder(args).Build();
        app.MapGet("/", () => "ok");
        {run}

        """.ReplaceLineEndings("\n");

    /// <summary>The same program with an explicit <c>Main</c> declared as <paramref name="signature"/>.</summary>
    private static string Entry(string signature, string run) => $$"""
        using System.Threading.Tasks;
        using Microsoft.AspNetCore.Builder;

        public static class Program
        {
            {{signature}}
            {
                var app = WebApplication.CreateBuilder(args).Build();
                {{run}}
            }
        }

        """.ReplaceLineEndings("\n");

    [Theory]
    [InlineData("await app.RunAsync();")]
    [InlineData("await app.RunAsync().ConfigureAwait(false);")]
    [InlineData("var running = app.RunAsync();\nawait running;")]
    public async Task an_awaited_host_is_accepted(string run)
    {
        Assert.Empty(await Application.AnalyzeAsync(Program(run), OutputKind.ConsoleApplication));
    }

    [Fact]
    public async Task a_host_task_returned_from_the_entry_point_is_accepted()
    {
        Assert.Empty(await Application.AnalyzeAsync(Entry("public static Task Main(string[] args)", "return app.RunAsync();"), OutputKind.ConsoleApplication));
    }

    [Theory]
    [InlineData("app.Run();", "ISOPLANE1020")]
    [InlineData("app.Run(\"http://localhost:5000\");", "ISOPLANE1020")]
    [InlineData("app.RunAsync();", "ISOPLANE1021")]
    [InlineData("app.RunAsync().Wait();", "ISOPLANE1021")]
    [InlineData("app.RunAsync().GetAwaiter().GetResult();", "ISOPLANE1021")]
    [InlineData("app.RunAsync().ConfigureAwait(false).GetAwaiter().GetResult();", "ISOPLANE1021")]
    public async Task a_host_that_is_not_awaited_is_reported_and_fixed_in_top_level_statements(string run, string id)
    {
        var source = Program(run);
        Assert.Equal([id], await Application.AnalyzeAsync(source, OutputKind.ConsoleApplication));
        var changed = await Application.FixAsync(source, id, OutputKind.ConsoleApplication);
        string expected = run.Contains("ConfigureAwait") ? "await app.RunAsync().ConfigureAwait(false);"
            : run.Contains("localhost") ? "await app.RunAsync(\"http://localhost:5000\");" : "await app.RunAsync();";
        Assert.Equal(Program(expected), changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!, OutputKind.ConsoleApplication));
    }

    [Theory]
    [InlineData("app.Run();", "ISOPLANE1020")]
    [InlineData("app.RunAsync().Wait();", "ISOPLANE1021")]
    public async Task the_fix_makes_a_synchronous_entry_point_asynchronous(string run, string id)
    {
        var source = Entry("public static void Main(string[] args)", run);
        Assert.Equal([id], await Application.AnalyzeAsync(source, OutputKind.ConsoleApplication));
        var changed = await Application.FixAsync(source, id, OutputKind.ConsoleApplication);
        Assert.Equal(Entry("public static async Task Main(string[] args)", "await app.RunAsync();"), changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!, OutputKind.ConsoleApplication));
    }

    [Fact]
    public async Task a_generic_host_is_held_to_the_same_rule()
    {
        const string source = """
            using Microsoft.Extensions.Hosting;

            var host = Host.CreateDefaultBuilder(args).Build();
            host.Run();

            """;
        Assert.Equal(["ISOPLANE1020"], await Application.AnalyzeAsync(source, OutputKind.ConsoleApplication));
        Assert.Equal(source.Replace("host.Run();", "await host.RunAsync();"), await Application.FixAsync(source, "ISOPLANE1020", OutputKind.ConsoleApplication));
    }

    [Fact]
    public async Task a_method_that_returns_a_value_is_reported_without_a_fix()
    {
        var source = Entry("public static int Main(string[] args)", "app.Run();\n        return 0;");
        Assert.Equal(["ISOPLANE1020"], await Application.AnalyzeAsync(source, OutputKind.ConsoleApplication));
        Assert.Null(await Application.FixAsync(source, "ISOPLANE1020", OutputKind.ConsoleApplication));
    }
}
