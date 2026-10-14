namespace Isoplane.Cli;

internal enum CodingAgent { None, Claude, Copilot, Cursor, Gemini }

/// <summary>
/// A variable a continuous integration service sets, and the name its runs are recorded under. Some
/// services set theirs to <c>true</c>; for the others, any value marks a run.
/// </summary>
internal sealed record CiService(string Variable, string Name, bool AnyValue);

/// <summary>An environment variable that changes telemetry, with its description in the telemetry reference.</summary>
internal sealed record TelemetryVariable(string Name, string Description);

/// <summary>
/// The environment variables telemetry reads. The CLI takes their names from here and the documentation
/// generator builds the reference page from here, so the page lists what is read.
/// </summary>
internal static class TelemetryVariables
{
    internal static readonly TelemetryVariable OptOut = new("ISOPLANE_TELEMETRY_OPTOUT",
        "Set to `1` or `true` to turn telemetry off. Nothing is recorded or sent while it is set.");

    internal static readonly TelemetryVariable DoNotTrack = new("DO_NOT_TRACK",
        "Set to `1` or `true` to turn telemetry off, as it does for other tools that read this variable.");

    internal static readonly TelemetryVariable Debug = new("ISOPLANE_TELEMETRY_DEBUG",
        "Set to `1` or `true` to print each event to standard error when a command ends. While it is set, nothing is recorded or sent, " +
        "so it shows what a command would report without reporting it.");

    internal static readonly TelemetryVariable TestRun = new("ISOPLANE_TELEMETRY_TEST_RUN",
        "Isoplane.Testing sets this to `1` on the CLI it starts, so that a session a test starts is recorded as one.");

    internal static readonly TelemetryVariable[] OptOuts = [OptOut, DoNotTrack];
    internal static readonly TelemetryVariable[] All = [OptOut, DoNotTrack, Debug, TestRun];

    /// <summary>
    /// The continuous integration services recognised by a variable each sets, checked in this order.
    /// <c>CI</c>, which most services set, marks a run on any other service.
    /// </summary>
    internal static readonly CiService[] CiServices =
    [
        new("GITHUB_ACTIONS", "github-actions", AnyValue: false), new("TF_BUILD", "azure-pipelines", AnyValue: false),
        new("GITLAB_CI", "gitlab", AnyValue: false), new("BITBUCKET_BUILD_NUMBER", "bitbucket", AnyValue: true),
        new("BUILDKITE", "buildkite", AnyValue: false), new("CIRCLECI", "circleci", AnyValue: false),
        new("TRAVIS", "travis", AnyValue: false), new("APPVEYOR", "appveyor", AnyValue: false),
        new("DRONE", "drone", AnyValue: false), new("TEAMCITY_VERSION", "teamcity", AnyValue: true),
        new("JENKINS_URL", "jenkins", AnyValue: true), new("CODEBUILD_BUILD_ID", "aws-codebuild", AnyValue: true),
    ];

    internal const string OtherCiService = "other";

    /// <summary>The name of the service running this process, or null when none is.</summary>
    internal static string? CiService(Func<string, string?> environment)
    {
        foreach (var service in CiServices)
            if (service.AnyValue ? environment(service.Variable) is { Length: > 0 } : IsOn(environment(service.Variable)))
                return service.Name;
        // Services outside the list set CI to values other than true, such as their own name.
        return environment("CI") is { Length: > 0 } value && value != "0" && !string.Equals(value, "false", StringComparison.OrdinalIgnoreCase)
            ? OtherCiService : null;
    }

    /// <summary>The variables the .NET CLI reads to name the agent running it.</summary>
    internal static readonly (string Variable, CodingAgent Agent)[] Agents =
    [
        ("CLAUDECODE", CodingAgent.Claude), ("GITHUB_COPILOT_CLI_MODE", CodingAgent.Copilot),
        ("CURSOR_EDITOR", CodingAgent.Cursor), ("GEMINI_CLI", CodingAgent.Gemini)
    ];

    internal static bool IsOn(string? value) => value is "1" || string.Equals(value, "true", StringComparison.OrdinalIgnoreCase);
}
