namespace Isoplane.Cli;

internal enum CodingAgent { None, Claude, Copilot, Cursor, Gemini }

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

    /// <summary>Set to <c>1</c> or <c>true</c> by continuous integration services.</summary>
    internal static readonly string[] CiFlags = ["CI", "TF_BUILD", "GITHUB_ACTIONS", "GITLAB_CI", "BUILDKITE", "CIRCLECI", "TRAVIS", "APPVEYOR"];

    /// <summary>Set to any value by continuous integration services.</summary>
    internal static readonly string[] CiMarkers = ["TEAMCITY_VERSION", "JENKINS_URL", "CODEBUILD_BUILD_ID"];

    /// <summary>The variables the .NET CLI reads to name the agent running it.</summary>
    internal static readonly (string Variable, CodingAgent Agent)[] Agents =
    [
        ("CLAUDECODE", CodingAgent.Claude), ("GITHUB_COPILOT_CLI_MODE", CodingAgent.Copilot),
        ("CURSOR_EDITOR", CodingAgent.Cursor), ("GEMINI_CLI", CodingAgent.Gemini)
    ];

    internal static bool IsOn(string? value) => value is "1" || string.Equals(value, "true", StringComparison.OrdinalIgnoreCase);
}
