using System.Threading.Tasks;

namespace Isoplane
{
    // Stand-ins for the SDK types the bundler recognises by name, so this fixture builds without the SDK.
    public sealed class HttpHandlerAttribute : System.Attribute { }
    public sealed class Request { }
    public sealed class Response { }
}

public static class Application
{
    [Isoplane.HttpHandler]
    public static Task<Isoplane.Response> HandleAsync(Isoplane.Request request) => Task.FromResult(new Isoplane.Response());

    public static string Dependency => global::Dependency.Value;
}
