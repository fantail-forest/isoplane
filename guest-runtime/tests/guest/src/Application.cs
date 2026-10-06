using System.Threading.Tasks;
using Isoplane;

namespace Acceptance;

/// <summary>The runtime acceptance application.</summary>
public static class Application
{
    private static AcceptanceHandler handler = null!;

    /// <summary>Constructs the behaviour families during initialization, so the template carries them.</summary>
    [Initializer]
    public static Task InitializeAsync()
    {
        handler = new AcceptanceHandler();
        return Task.CompletedTask;
    }

    /// <summary>Serves the runtime behaviour probes.</summary>
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) => handler.HandleAsync(request);
}
