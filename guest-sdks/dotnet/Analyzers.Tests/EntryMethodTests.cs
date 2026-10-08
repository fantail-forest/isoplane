using Isoplane.Build;
using Microsoft.CodeAnalysis;
using Xunit;

namespace Isoplane.Analyzers.Tests;

public sealed class EntryMethodTests
{
    private const string Handler = "[HttpHandler] public static Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));";

    /// <summary>A handler application's members, and the diagnostics they must draw.</summary>
    public static TheoryData<string, string[]> Cases => new()
    {
        { Handler, [] },
        { "[HttpHandler] public static Task<Response> HandleAsync(Request request, Ctx ctx) => Task.FromResult(Response.Text(\"ok\"));", [] },
        { "[HttpHandler] public static async Task<Response> HandleAsync(StreamingRequest request) { await Task.Yield(); return Response.Text(\"ok\"); }", [] },
        { "[HttpHandler] internal static Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));", [] },
        { Handler + "\n[Initializer] public static Task InitializeAsync() => Task.CompletedTask;\n[Activated] private static Task ActivatedAsync() => Task.CompletedTask;", [] },

        { "[HttpHandler] public static Response Handle(Request request) => Response.Text(\"ok\");", ["ISOPLANE1011"] },
        { "[HttpHandler] public static ValueTask<Response> HandleAsync(Request request) => new(Response.Text(\"ok\"));", ["ISOPLANE1011"] },
        { "[HttpHandler] public static Task HandleAsync(Request request) => Task.CompletedTask;", ["ISOPLANE1011"] },
        { "[HttpHandler] public static Task<Response> HandleAsync() => Task.FromResult(Response.Text(\"ok\"));", ["ISOPLANE1012"] },
        { "[HttpHandler] public static Task<Response> HandleAsync(Request request, string extra) => Task.FromResult(Response.Text(\"ok\"));", ["ISOPLANE1012"] },
        { "[HttpHandler] public static Task<Response> HandleAsync(Ctx ctx, Request request) => Task.FromResult(Response.Text(\"ok\"));", ["ISOPLANE1012"] },
        { "[HttpHandler] public static Task<Response> HandleAsync(ref Request request) => Task.FromResult(Response.Text(\"ok\"));", ["ISOPLANE1012"] },
        { "[HttpHandler] public static Task<Response> HandleAsync<T>(Request request) => Task.FromResult(Response.Text(\"ok\"));", ["ISOPLANE1015"] },
        { Handler + "\npublic static Task<Response> HandleAsync(StreamingRequest request) => Task.FromResult(Response.Text(\"ok\"));", ["ISOPLANE1016"] },
        { Handler + "\n[HttpHandler] public static Task<Response> OtherAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));", ["ISOPLANE1017", "ISOPLANE1017"] },

        { Handler + "\n[Initializer] public static void Initialize() { }", ["ISOPLANE1013"] },
        { Handler + "\n[Initializer] public static async void Initialize() { await Task.Yield(); }", ["ISOPLANE1013"] },
        { Handler + "\n[Activated] public static ValueTask ActivatedAsync() => default;", ["ISOPLANE1013"] },
        { Handler + "\n[Activated] public static Task<int> ActivatedAsync() => Task.FromResult(1);", ["ISOPLANE1013"] },
        { Handler + "\n[Initializer] public static Task InitializeAsync(int count) => Task.CompletedTask;", ["ISOPLANE1014"] },
        { Handler + "\n[Initializer] public static void Initialize(int count) { }", ["ISOPLANE1013", "ISOPLANE1014"] },
        { Handler + "\n[Activated] public static Task A() => Task.CompletedTask;\n[Activated] public static Task B() => Task.CompletedTask;", ["ISOPLANE1017", "ISOPLANE1017"] },
    };

    [Theory]
    [MemberData(nameof(Cases))]
    public async Task each_requirement_an_entry_method_misses_is_reported(string members, string[] expected)
    {
        Assert.Equal(expected, await Application.AnalyzeAsync(Application.Library(members)));
    }

    [Theory]
    [MemberData(nameof(Cases))]
    public async Task the_analyzer_and_the_build_check_of_the_assembly_agree(string members, string[] expected)
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-entry-agreement-");
        try
        {
            var assembly = Path.Combine(directory.FullName, "App.dll");
            var emitted = (await Application.Project(Application.Library(members)).GetCompilationAsync())!.Emit(assembly);
            Assert.True(emitted.Success, string.Join("; ", emitted.Diagnostics));
            var refusal = Record.Exception(() => ApplicationEntryDiscovery.Discover(assembly));
            Assert.True(refusal is null or ApplicationEntryException, refusal?.ToString());
            Assert.True(refusal is null == (expected.Length == 0),
                $"the analyzer reports [{string.Join(", ", expected)}] and the build check says: {refusal?.Message ?? "nothing"}");
        }
        finally { directory.Delete(recursive: true); }
    }

    [Theory]
    [InlineData("public sealed class App\n{\n    [HttpHandler] public Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));\n}", "ISOPLANE1010")]
    [InlineData("public static class App<T>\n{\n    [HttpHandler] public static Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));\n}", "ISOPLANE1015")]
    public async Task requirements_of_the_declaring_type_are_reported_and_agree(string type, string expected)
    {
        string source = "using System.Threading.Tasks;\nusing Isoplane;\n" + type + "\n";
        Assert.Equal([expected], await Application.AnalyzeAsync(source));
        var directory = Directory.CreateTempSubdirectory("isoplane-entry-agreement-");
        try
        {
            var assembly = Path.Combine(directory.FullName, "App.dll");
            Assert.True((await Application.Project(source).GetCompilationAsync())!.Emit(assembly).Success);
            Assert.Throws<ApplicationEntryException>(() => ApplicationEntryDiscovery.Discover(assembly));
        }
        finally { directory.Delete(recursive: true); }
    }

    [Fact]
    public async Task an_attribute_in_a_project_with_an_entry_point_is_reported_and_agrees()
    {
        const string source = """
            using System.Threading.Tasks;
            using Isoplane;

            public static class Program
            {
                public static async Task Main() => await Task.Yield();

                [Initializer] public static Task InitializeAsync() => Task.CompletedTask;
            }

            """;
        Assert.Equal(["ISOPLANE1018"], await Application.AnalyzeAsync(source, OutputKind.ConsoleApplication));
        var directory = Directory.CreateTempSubdirectory("isoplane-entry-agreement-");
        try
        {
            var assembly = Path.Combine(directory.FullName, "App.dll");
            Assert.True((await Application.Project(source, OutputKind.ConsoleApplication).GetCompilationAsync())!.Emit(assembly).Success);
            Assert.Throws<ApplicationEntryException>(() => ApplicationEntryDiscovery.Discover(assembly));
        }
        finally { directory.Delete(recursive: true); }
    }

    [Fact]
    public async Task a_project_that_does_not_use_the_sdk_is_not_analyzed()
    {
        Assert.Empty(await Application.AnalyzeAsync("public static class App { public static void Run() { } }\n"));
    }
}
