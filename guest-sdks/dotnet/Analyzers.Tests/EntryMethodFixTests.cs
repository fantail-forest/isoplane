using Microsoft.CodeAnalysis;
using Xunit;

namespace Isoplane.Analyzers.Tests;

public sealed class EntryMethodFixTests
{
    private const string Handler = "[HttpHandler] public static Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));";

    /// <summary>The members before a fix, the diagnostic fixed, and the members afterwards.</summary>
    public static TheoryData<string, string, string> Fixes => new()
    {
        {
            "[HttpHandler] public static Response Handle(Request request) => Response.Text(\"ok\");",
            "ISOPLANE1011",
            "[HttpHandler] public static Task<Response> Handle(Request request) => Task.FromResult(Response.Text(\"ok\"));"
        },
        {
            """
            [HttpHandler]
            public static Response Handle(Request request)
            {
                Func<Response> missing = () => { return Response.Text("no", 404); };
                if (request.Path != "/") return missing();
                return Response.Text("ok");
            }
            """,
            "ISOPLANE1011",
            """
            [HttpHandler]
            public static Task<Response> Handle(Request request)
            {
                Func<Response> missing = () => { return Response.Text("no", 404); };
                if (request.Path != "/") return Task.FromResult(missing());
                return Task.FromResult(Response.Text("ok"));
            }
            """
        },
        {
            "[HttpHandler] public static async ValueTask<Response> HandleAsync(Request request) { await Task.Yield(); return Response.Text(\"ok\"); }",
            "ISOPLANE1011",
            "[HttpHandler] public static async Task<Response> HandleAsync(Request request) { await Task.Yield(); return Response.Text(\"ok\"); }"
        },
        {
            "[HttpHandler] public static Task<Response> HandleAsync() => Task.FromResult(Response.Text(\"ok\"));",
            "ISOPLANE1012",
            "[HttpHandler] public static Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));"
        },
        {
            Handler + """

            [Initializer]
            public static void Initialize()
            {
                if (Environment.ProcessorCount == 0) return;
                Console.WriteLine("ready");
            }
            """,
            "ISOPLANE1013",
            Handler + """

            [Initializer]
            public static Task Initialize()
            {
                if (Environment.ProcessorCount == 0) return Task.CompletedTask;
                Console.WriteLine("ready");
                return Task.CompletedTask;
            }
            """
        },
        {
            Handler + "\n[Initializer] public static void Initialize() => Console.WriteLine(\"ready\");",
            "ISOPLANE1013",
            Handler + """

            [Initializer] public static Task Initialize()
            {
                Console.WriteLine("ready");
                return Task.CompletedTask;
            }
            """
        },
        {
            Handler + "\n[Activated] public static async void Activate() { await Task.Yield(); }",
            "ISOPLANE1013",
            Handler + "\n[Activated] public static async Task Activate() { await Task.Yield(); }"
        },
        {
            Handler + "\n[Initializer] public static Task InitializeAsync(int count, string name) => Task.CompletedTask;",
            "ISOPLANE1014",
            Handler + "\n[Initializer] public static Task InitializeAsync() => Task.CompletedTask;"
        },
        {
            Handler + "\n[HttpHandler] public static Task<Response> OtherAsync(Request request) => Task.FromResult(Response.Text(\"other\"));",
            "ISOPLANE1017",
            "public static Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));\n[HttpHandler] public static Task<Response> OtherAsync(Request request) => Task.FromResult(Response.Text(\"other\"));"
        },
    };

    [Theory]
    [MemberData(nameof(Fixes))]
    public async Task a_fix_leaves_the_method_meeting_its_attribute(string before, string id, string after)
    {
        var changed = await Application.FixAsync(Application.Library(before), id);
        Assert.Equal(Application.Library(after), changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!));
    }

    [Fact]
    public async Task an_instance_method_is_made_static()
    {
        const string before = "using System.Threading.Tasks;\nusing Isoplane;\npublic sealed class App\n{\n    [HttpHandler]\n    public Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text(\"ok\"));\n}\n";
        var changed = await Application.FixAsync(before, "ISOPLANE1010");
        Assert.Equal(before.Replace("public Task<Response>", "public static Task<Response>"), changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!));
    }

    [Fact]
    public async Task an_attribute_that_cannot_apply_is_removed()
    {
        const string before = "using System.Threading.Tasks;\nusing Isoplane;\npublic static class Program\n{\n    public static async Task Main() => await Task.Yield();\n\n    [Initializer]\n    public static Task InitializeAsync() => Task.CompletedTask;\n}\n";
        var changed = await Application.FixAsync(before, "ISOPLANE1018", OutputKind.ConsoleApplication);
        Assert.Equal(before.Replace("    [Initializer]\n", ""), changed);
        Assert.Empty(await Application.AnalyzeAsync(changed!, OutputKind.ConsoleApplication));
    }

    /// <summary>Methods with more than one reasonable repair, or none that keeps their code compiling.</summary>
    [Theory]
    [InlineData("[HttpHandler] public static ValueTask<Response> HandleAsync(Request request) => new(Response.Text(\"ok\"));", "ISOPLANE1011")]
    [InlineData("[HttpHandler] public static Task<Response> HandleAsync(Request request, string extra) => Task.FromResult(Response.Text(\"ok\"));", "ISOPLANE1012")]
    [InlineData(Handler + "\n[Activated] public static ValueTask ActivatedAsync() => default;", "ISOPLANE1013")]
    [InlineData(Handler + "\n[Initializer] public static Task InitializeAsync(int count) => Task.Delay(count);", "ISOPLANE1014")]
    public async Task a_diagnostic_without_one_clear_repair_offers_none(string members, string id)
    {
        Assert.Null(await Application.FixAsync(Application.Library(members), id));
    }
}
