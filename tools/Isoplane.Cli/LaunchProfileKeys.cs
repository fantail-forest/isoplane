namespace Isoplane.Cli;

/// <summary>A key Isoplane reads from a launch profile, with its description in the launch settings reference.</summary>
internal sealed record LaunchProfileKey(string Name, string Description);

/// <summary>
/// A key of a launch profile's limit overrides: the manifest resource limit it sets, or none for the
/// memory limit, which the manifest carries on its own.
/// </summary>
internal sealed record LimitOverrideKey(string Name, string? ResourceLimit, string Description);

/// <summary>
/// The keys Isoplane reads from <c>Properties/launchSettings.json</c>. The parser takes their names from
/// here and the documentation generator builds the reference page from here, so the page lists what is read.
/// </summary>
internal static class LaunchProfileKeys
{
    internal static readonly LaunchProfileKey Command = new("commandName",
        "Must be `Project` for Isoplane to use the profile. Profiles with another value, such as `IISExpress`, are passed over.");

    internal static readonly LaunchProfileKey Url = new("applicationUrl",
        "Sets the port a local session listens on: the port of the first `http` address. Separate several addresses with `;`. " +
        "A local session serves plain HTTP on loopback, so the host name is not used and `https` addresses are passed over.");

    internal static readonly LaunchProfileKey Environment = new("environmentVariables",
        "An object whose entries become application settings. Two underscores in a name stand for a colon, so `Greeting__Default` " +
        "is the setting `Greeting:Default`. `ASPNETCORE_ENVIRONMENT`, or `DOTNET_ENVIRONMENT`, names the environment of an ASP.NET application.");

    internal static readonly LaunchProfileKey Limits = new("isoplaneLimitsOverride",
        "An object that overrides the default limits a local session runs with. Its keys are listed below.");

    internal static readonly LaunchProfileKey[] Profile = [Command, Url, Environment, Limits];

    internal static readonly LimitOverrideKey Memory = new("memoryBytes", null,
        "The instance soft memory limit, in bytes, with a 25% hard-limit margin for finishing work. Must be a multiple of 65536.");

    internal static readonly LimitOverrideKey[] LimitOverrides =
    [
        Memory,
        new("occupancyMsPerDispatch", "occupancy_ms_per_dispatch",
            "The running time one request may use, in milliseconds. Waiting does not count. Background work uses a separate instance allowance."),
        new("concurrentConnectionsPerInstance", "instance_open_connections",
            "The most outbound connections one instance may hold concurrently, including connections being opened and pooled connections. Closing a connection frees capacity. The host network ceiling still applies."),
        new("pendingFuturesPerDispatch", "pending_futures_per_dispatch",
            "The most asynchronous operations one dispatch may have pending at once."),
    ];
}
