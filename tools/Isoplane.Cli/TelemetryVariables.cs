namespace Isoplane.Cli;

internal enum CodingAgent
{
    None, Cowork, Claude, Cursor, Gemini, CopilotCli, CopilotApp, CopilotVscode, Codex, Aider, Plandex, Amp, Qwen, Droid,
    Opencode, Zed, Kimi, Openhands, Goose, Cline, Roo, Windsurf, Replit, Augment, Antigravity, Pi, Other
}

/// <summary>
/// Environment variables that name the coding agent running a process: any of <paramref name="Variables"/>
/// set to anything, or set to <paramref name="Value"/> when one is given. Only the match is recorded.
/// </summary>
internal sealed record AgentRule(CodingAgent Agent, string[] Variables, string? Value = null)
{
    /// <summary>The name the agent is recorded under, as the event's JSON spells it.</summary>
    internal string Name => System.Text.Json.JsonNamingPolicy.KebabCaseLower.ConvertName(Agent.ToString());

    internal bool Matches(Func<string, string?> environment) => Variables.Any(variable => environment(variable) is { Length: > 0 } set
        && (Value is null || string.Equals(set, Value, StringComparison.OrdinalIgnoreCase)));
}

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

    /// <summary>
    /// The rules that name the coding agent running a command, checked in order; the first that matches
    /// decides. They follow the Aspire CLI's detector
    /// (https://github.com/dotnet/aspire/blob/main/src/Aspire.Cli/Telemetry/CodingAgentDetector.cs), which
    /// is maintained as agents change their variables, with Pi added from its own documentation. Aspire also
    /// counts <c>CURSOR_EDITOR</c>, which is left out: it can be set in Cursor's terminal when a person types.
    /// </summary>
    internal static readonly AgentRule[] Agents =
    [
        new(CodingAgent.Cowork, ["CLAUDE_CODE_IS_COWORK"]),
        new(CodingAgent.Claude, ["CLAUDECODE", "CLAUDE_CODE", "CLAUDE_CODE_ENTRYPOINT"]),
        new(CodingAgent.Cursor, ["CURSOR_AI", "CURSOR_TRACE_ID", "CURSOR_AGENT"]),
        new(CodingAgent.Gemini, ["GEMINI_CLI"]),
        new(CodingAgent.CopilotCli, ["COPILOT_CLI", "GITHUB_COPILOT_CLI_MODE", "GH_COPILOT_WORKING_DIRECTORY", "COPILOT_MODEL", "COPILOT_ALLOW_ALL", "COPILOT_GITHUB_TOKEN"]),
        new(CodingAgent.CopilotApp, ["AI_AGENT"], "github_copilot_app_agent"),
        new(CodingAgent.CopilotVscode, ["AI_AGENT"], "github_copilot_vscode_agent"),
        new(CodingAgent.CopilotVscode, ["COPILOT_AGENT"]),
        new(CodingAgent.Codex, ["CODEX_CLI", "CODEX_SANDBOX", "CODEX_CI", "CODEX_THREAD_ID"]),
        new(CodingAgent.Aider, ["OR_APP_NAME"], "Aider"),
        new(CodingAgent.Plandex, ["OR_APP_NAME"], "plandex"),
        new(CodingAgent.Amp, ["AMP_HOME"]),
        new(CodingAgent.Qwen, ["QWEN_CODE"]),
        new(CodingAgent.Droid, ["DROID_CLI"]),
        new(CodingAgent.Opencode, ["OPENCODE_AI"]),
        new(CodingAgent.Zed, ["ZED_ENVIRONMENT", "ZED_TERM"]),
        new(CodingAgent.Kimi, ["KIMI_CLI"]),
        new(CodingAgent.Openhands, ["OR_APP_NAME"], "OpenHands"),
        new(CodingAgent.Goose, ["GOOSE_TERMINAL", "GOOSE_PROVIDER"]),
        new(CodingAgent.Cline, ["CLINE_TASK_ID"]),
        new(CodingAgent.Roo, ["ROO_CODE_TASK_ID"]),
        new(CodingAgent.Windsurf, ["WINDSURF_SESSION"]),
        new(CodingAgent.Replit, ["REPL_ID"]),
        new(CodingAgent.Augment, ["AUGMENT_AGENT"]),
        new(CodingAgent.Antigravity, ["ANTIGRAVITY_AGENT"]),
        new(CodingAgent.Pi, ["PI_CODING_AGENT"]),
        new(CodingAgent.Other, ["AGENT_CLI"]),
    ];

    internal static bool IsOn(string? value) => value is "1" || string.Equals(value, "true", StringComparison.OrdinalIgnoreCase);
}
