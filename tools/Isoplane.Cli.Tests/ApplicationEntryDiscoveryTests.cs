using Isoplane.Build;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class ApplicationEntryDiscoveryTests : IDisposable
{
    private readonly string directory = Directory.CreateTempSubdirectory("isoplane-application-entry-").FullName;
    private readonly string sdk;

    public ApplicationEntryDiscoveryTests() => sdk = Compile("Isoplane.Sdk", OutputKind.DynamicallyLinkedLibrary, """
        namespace Isoplane;
        public sealed class HttpHandlerAttribute : System.Attribute;
        public sealed class InitializerAttribute : System.Attribute;
        public sealed class ActivatedAttribute : System.Attribute;
        public sealed class Request;
        public sealed class StreamingRequest;
        public sealed class Response;
        public sealed class Ctx;
        """);

    [Theory]
    [InlineData("await System.Threading.Tasks.Task.Yield();", "Program", "<Main>$")]
    [InlineData("static class Start { static async System.Threading.Tasks.Task Main() => await System.Threading.Tasks.Task.Yield(); }", "Start", "Main")]
    [InlineData("namespace App { static class Start { static async System.Threading.Tasks.Task<int> Main(string[] args) { await System.Threading.Tasks.Task.Yield(); return 0; } } }", "App.Start", "Main")]
    public void an_executable_is_entered_through_the_asynchronous_method_its_entry_point_wraps(string source, string type, string method)
    {
        var entry = Assert.IsType<EntryPointApplication>(ApplicationEntryDiscovery.Discover(Compile("App", OutputKind.ConsoleApplication, source)));
        Assert.Equal(new MethodReference(type, method), entry.Main);
    }

    [Theory]
    [InlineData("System.Console.WriteLine();")]
    [InlineData("static class Start { static void Main() { } }")]
    public void a_synchronous_entry_point_is_refused_with_the_await_it_needs(string source)
    {
        var failure = Assert.Throws<ApplicationEntryException>(() => ApplicationEntryDiscovery.Discover(Compile("App", OutputKind.ConsoleApplication, source)));
        Assert.Contains("await app.RunAsync();", failure.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void a_library_is_entered_through_its_attributed_methods()
    {
        var entry = Assert.IsType<HandlerApplication>(ApplicationEntryDiscovery.Discover(Compile("App", OutputKind.DynamicallyLinkedLibrary, """
            using System.Threading.Tasks;
            using Isoplane;
            namespace App;
            static class Api
            {
                [HttpHandler] static Task<Response> HandleAsync(Request request, Ctx context) => throw null!;
                static class Lifecycle
                {
                    [Initializer] internal static Task InitializeAsync() => Task.CompletedTask;
                    [Activated] internal static Task ActivatedAsync() => Task.CompletedTask;
                }
            }
            """)));
        Assert.Equal(new MethodReference("App.Api", "HandleAsync"), entry.Http);
        Assert.Equal(new MethodReference("App.Api+Lifecycle", "InitializeAsync"), entry.Initializer);
        Assert.Equal(new MethodReference("App.Api+Lifecycle", "ActivatedAsync"), entry.Activated);
    }

    [Theory]
    [InlineData("Task<Response> HandleAsync(Request request)")]
    [InlineData("Task<Response> HandleAsync(StreamingRequest request)")]
    [InlineData("Task<Response> HandleAsync(StreamingRequest request, Ctx context)")]
    public void a_handler_may_omit_the_context_or_stream_its_request(string signature)
    {
        var entry = Assert.IsType<HandlerApplication>(ApplicationEntryDiscovery.Discover(Library($"[HttpHandler] public static {signature} => throw null!;")));
        Assert.Equal(new MethodReference("Api", "HandleAsync"), entry.Http);
        Assert.Null(entry.Initializer);
        Assert.Null(entry.Activated);
    }

    [Theory]
    [InlineData("public static Task<Response> HandleAsync(Request request) => throw null!;", "declares no application entry")]
    [InlineData("[HttpHandler] public static Task<Response> One(Request request) => throw null!; [HttpHandler] public static Task<Response> Two(Request request) => throw null!;", "marks 2: Api.One, Api.Two")]
    [InlineData("[HttpHandler] public Task<Response> HandleAsync(Request request) => throw null!;", "must be a static, non-generic method")]
    [InlineData("[HttpHandler] public static Task<Response> HandleAsync<T>(Request request) => throw null!;", "must be a static, non-generic method")]
    [InlineData("[HttpHandler] public static Response HandleAsync(Request request) => throw null!;", "must return Task<Response>")]
    [InlineData("[HttpHandler] public static Task<Response> HandleAsync(string path) => throw null!;", "must return Task<Response>")]
    [InlineData("[HttpHandler] public static Task<Response> HandleAsync(Request request, Ctx context, int more) => throw null!;", "must return Task<Response>")]
    [InlineData("[HttpHandler] public static Task<Response> HandleAsync(Request request) => throw null!; static Task<Response> HandleAsync(Request request, int more) => throw null!;", "must not be overloaded")]
    [InlineData("[HttpHandler] public static Task<Response> HandleAsync(Request request) => throw null!; [Initializer] public static void Initialize() { }", "[Initializer] method 'Api.Initialize' must return Task")]
    [InlineData("[HttpHandler] public static Task<Response> HandleAsync(Request request) => throw null!; [Activated] public static Task Activated(int instance) => Task.CompletedTask;", "[Activated] method 'Api.Activated' must return Task")]
    [InlineData("[HttpHandler] public static Task<Response> HandleAsync(Request request) => throw null!; [Initializer] public static Task One() => Task.CompletedTask; [Initializer] public static Task Two() => Task.CompletedTask;", "one [Initializer] method")]
    public void a_library_without_exactly_one_usable_entry_is_refused_by_cause(string members, string cause)
    {
        var failure = Assert.Throws<ApplicationEntryException>(() => ApplicationEntryDiscovery.Discover(Library(members)));
        Assert.Contains(cause, failure.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void an_executable_with_attributed_methods_is_refused_because_they_would_never_run()
    {
        var failure = Assert.Throws<ApplicationEntryException>(() => ApplicationEntryDiscovery.Discover(Compile("App", OutputKind.ConsoleApplication, """
            await System.Threading.Tasks.Task.Yield();
            static class Api { [Isoplane.Initializer] static System.Threading.Tasks.Task InitializeAsync() => System.Threading.Tasks.Task.CompletedTask; }
            """)));
        Assert.Contains("has an entry point, so its [Initializer] method 'Api.InitializeAsync' would never be called", failure.Message, StringComparison.Ordinal);
    }

    private string Library(string members) => Compile("App", OutputKind.DynamicallyLinkedLibrary,
        "using System.Threading.Tasks; using Isoplane; public class Api { " + members + " }");

    private string Compile(string name, OutputKind kind, string source)
    {
        var output = Directory.CreateDirectory(Path.Combine(directory, Guid.NewGuid().ToString("N")));
        var references = new[] { typeof(object).Assembly.Location, typeof(Console).Assembly.Location, Path.Combine(Path.GetDirectoryName(typeof(object).Assembly.Location)!, "System.Runtime.dll") }
            .Concat(sdk is null ? [] : [sdk]).Select(path => MetadataReference.CreateFromFile(path));
        var compilation = CSharpCompilation.Create(name, [CSharpSyntaxTree.ParseText(source)], references, new CSharpCompilationOptions(kind));
        var path = Path.Combine(output.FullName, name + ".dll");
        using var target = File.Create(path);
        var result = compilation.Emit(target);
        Assert.True(result.Success, "The entry fixture must compile: " + string.Join("\n", result.Diagnostics));
        return path;
    }

    public void Dispose() => Directory.Delete(directory, recursive: true);
}
