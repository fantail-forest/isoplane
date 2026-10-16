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
        "is the setting `Greeting:Default`. `ASPNETCORE_ENVIRONMENT` names the environment of an ASP.NET application.");

    internal static readonly LaunchProfileKey Limits = new("isoplaneLimitsOverride",
        "An object that lowers the limits a local session runs with. Its keys are listed below.");

    internal static readonly LaunchProfileKey[] Profile = [Command, Url, Environment, Limits];

    internal static readonly LimitOverrideKey Memory = new("memoryBytes", null,
        "The most memory an instance may use, in bytes. Must be a multiple of 65536.");

    internal static readonly LimitOverrideKey[] LimitOverrides =
    [
        Memory,
        new("occupancyMsPerDispatch", "occupancy_ms_per_dispatch",
            "The running time one dispatch may use, in milliseconds. Time spent waiting for something the dispatch awaits does not count."),
        new("subrequestsPerDispatch", "subrequests_per_dispatch",
            "The most outbound requests one dispatch may start. Not yet enforced for .NET applications."),
        new("pendingFuturesPerDispatch", "pending_futures_per_dispatch",
            "The most asynchronous operations one dispatch may have pending at once."),
    ];
}
