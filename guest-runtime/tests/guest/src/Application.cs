using Isoplane;

namespace Acceptance;

/// <summary>Constructs the runtime acceptance application.</summary>
public static class Application
{
    /// <summary>Creates the handler serving the runtime behavior probes.</summary>
    public static IIsoplaneHandler Create() => new AcceptanceHandler();
}
