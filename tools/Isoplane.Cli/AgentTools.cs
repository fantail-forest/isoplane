using System.ComponentModel;
using System.Text.Json.Nodes;
using ModelContextProtocol;
using ModelContextProtocol.Server;

namespace Isoplane.Cli;

/// <summary>What a coding agent can do on this machine through the agent server.</summary>
[McpServerToolType]
internal sealed class AgentTools(AgentSessions sessions, CommandTelemetry telemetry)
{
    [McpServerTool(Name = AgentToolCatalogue.CheckMachine, Title = "Check this machine", ReadOnly = true, OpenWorld = false)]
    [Description(AgentToolCatalogue.CheckMachineDescription)]
    public async Task<string> CheckMachineAsync(CancellationToken cancellation)
    {
        var run = telemetry.ToolCall(TelemetryCommand.Doctor);
        try
        {
            var ready = await EnvironmentDoctor.CheckAsync(TextWriter.Null, cancellation);
            run.Ended(0);
            return ready;
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            run.Cancelled(0);
            throw;
        }
        catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            run.Failed(error, 1);
            throw new McpException(error.Message);
        }
        catch (Exception) when (run.Crashed())
        {
            throw;
        }
    }

    [McpServerTool(Name = AgentToolCatalogue.StartSession, Title = "Run an application", OpenWorld = false)]
    [Description(AgentToolCatalogue.StartSessionDescription)]
    public async Task<SessionStarted> StartSessionAsync(
        [Description("The directory holding the application's project file.")] string projectDirectory,
        [Description("The build configuration.")] string configuration = "Debug",
        [Description("The local HTTP port; the launch profile's port, or a free one, when omitted.")] int? port = null,
        [Description("The launch profile from Properties/launchSettings.json to use instead of the first.")] string? launchProfile = null,
        [Description("Whether a saved source change rebuilds and reloads the application.")] bool watch = true,
        CancellationToken cancellation = default)
    {
        var arguments = new List<string> { "--project", projectDirectory, "--configuration", configuration, "--json" };
        if (port is { } chosen) arguments.AddRange(["--port", chosen.ToString(System.Globalization.CultureInfo.InvariantCulture)]);
        if (launchProfile is not null) arguments.AddRange(["--launch-profile", launchProfile]);
        if (!watch) arguments.Add("--no-watch");
        try
        {
            var session = await sessions.StartAsync(DevelopmentOptions.Parse([.. arguments]), cancellation);
            return new SessionStarted(session.Id, session.Application, session.Url, session.ProjectDirectory);
        }
        catch (Exception error) when (error is ArgumentException or InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            throw new McpException(error.Message);
        }
    }

    [McpServerTool(Name = AgentToolCatalogue.SessionEvents, Title = "Read a session's events", ReadOnly = true, OpenWorld = false)]
    [Description(AgentToolCatalogue.SessionEventsDescription)]
    public async Task<SessionEvents> SessionEventsAsync(
        [Description("The session id start_session returned.")] string sessionId,
        [Description("The number of the last event already seen; 0 for all.")] long after = 0,
        [Description("How many seconds to wait for an event when there is none newer than `after`; at most 60.")] int waitSeconds = 0,
        CancellationToken cancellation = default)
    {
        var session = sessions.Find(sessionId) ?? throw new McpException($"No running session has the id '{sessionId}'. list_sessions shows the running ones.");
        var events = await session.Events.ReadAsync(after, TimeSpan.FromSeconds(Math.Clamp(waitSeconds, 0, 60)), cancellation);
        return new SessionEvents(events.Select(entry => new NumberedEvent(entry.Number, entry.Event)).ToArray(),
            events.Length > 0 ? events[^1].Number : after, session.Running);
    }

    [McpServerTool(Name = AgentToolCatalogue.StopSession, Title = "Stop an application", OpenWorld = false)]
    [Description(AgentToolCatalogue.StopSessionDescription)]
    public async Task<string> StopSessionAsync([Description("The session id start_session returned.")] string sessionId) =>
        await sessions.StopAsync(sessionId) ? $"Session {sessionId} stopped."
            : throw new McpException($"No running session has the id '{sessionId}'. list_sessions shows the running ones.");

    [McpServerTool(Name = AgentToolCatalogue.ListSessions, Title = "List running applications", ReadOnly = true, OpenWorld = false)]
    [Description(AgentToolCatalogue.ListSessionsDescription)]
    public SessionStarted[] ListSessions() =>
        sessions.All.Where(session => session.Running).Select(session => new SessionStarted(session.Id, session.Application, session.Url, session.ProjectDirectory)).ToArray();
}

/// <summary>A running session: its id, the application's name, its address and its project.</summary>
internal sealed record SessionStarted(string SessionId, string Application, string Url, string ProjectDirectory);

/// <summary>One event a session reported, with its number.</summary>
internal sealed record NumberedEvent(long Number, JsonObject Event);

/// <summary>The events read, the number to pass as `after` next time, and whether the session still runs.</summary>
internal sealed record SessionEvents(NumberedEvent[] Events, long Last, bool Running);
