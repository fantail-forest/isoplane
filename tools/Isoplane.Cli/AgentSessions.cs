using System.Collections.Concurrent;
using System.Security.Cryptography;
using System.Text.Json.Nodes;
using Microsoft.Extensions.Hosting;

namespace Isoplane.Cli;

/// <summary>A session's events in order, numbered from one, kept for a reader that polls.</summary>
internal sealed class EventLog
{
    private const int Capacity = 2000;
    private readonly object access = new();
    private readonly LinkedList<(long Number, JsonObject Event)> events = new();
    private long next = 1;
    private TaskCompletionSource arrived = new(TaskCreationOptions.RunContinuationsAsynchronously);

    internal void Append(JsonObject @event)
    {
        lock (access)
        {
            events.AddLast((next++, @event));
            if (events.Count > Capacity) events.RemoveFirst();
            arrived.TrySetResult();
            arrived = new(TaskCreationOptions.RunContinuationsAsynchronously);
        }
    }

    /// <summary>The events numbered above <paramref name="after"/>, waiting up to <paramref name="wait"/> for one when there are none.</summary>
    internal async Task<(long Number, JsonObject Event)[]> ReadAsync(long after, TimeSpan wait, CancellationToken cancellation)
    {
        var deadline = wait > TimeSpan.Zero ? Task.Delay(wait, cancellation) : Task.CompletedTask;
        while (true)
        {
            Task pending;
            lock (access)
            {
                var newer = events.Where(entry => entry.Number > after).ToArray();
                if (newer.Length > 0 || deadline.IsCompleted) return newer;
                pending = arrived.Task;
            }
            if (await Task.WhenAny(pending, deadline) == deadline) return [];
        }
    }
}

/// <summary>
/// One application running for a coding agent, recorded as a <c>dev</c> run that ends as the session
/// does: cancelled when the agent or the server stops it, as Ctrl+C ends <c>isoplane dev</c>.
/// </summary>
internal sealed record AgentSession(string Id, string ProjectDirectory, string Application, string Url, DevelopmentSession Session, EventLog Events, CommandTelemetry Run, Task Ended)
{
    internal bool Running => !Session.Completion.IsCompleted;
}

/// <summary>The sessions the agent server has started and not yet stopped; all stop when the server does.</summary>
internal sealed class AgentSessions(CommandTelemetry telemetry) : IHostedService
{
    private readonly ConcurrentDictionary<string, AgentSession> sessions = new(StringComparer.Ordinal);

    internal IReadOnlyCollection<AgentSession> All => sessions.Values.ToArray();

    internal AgentSession? Find(string id) => sessions.GetValueOrDefault(id);

    /// <summary>Builds and starts the project, then waits for the application to be ready.</summary>
    internal async Task<AgentSession> StartAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var run = telemetry.ToolCall(TelemetryCommand.Dev);
        try
        {
            var started = await StartSessionAsync(options, run, cancellation);
            sessions[started.Id] = started;
            return started;
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            run.Cancelled(0);
            throw;
        }
        catch (Exception error) when (error is ArgumentException or InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            run.Failed(error, 1);
            throw;
        }
        catch (Exception) when (run.Crashed())
        {
            throw;
        }
    }

    private static async Task<AgentSession> StartSessionAsync(DevelopmentOptions options, CommandTelemetry run, CancellationToken cancellation)
    {
        var events = new EventLog();
        var ready = new TaskCompletionSource<JsonObject>(TaskCreationOptions.RunContinuationsAsynchronously);
        var session = await DevelopmentSession.StartAsync(options, message =>
        {
            events.Append(message);
            if (message["event"]?.GetValue<string>() == "ready") ready.TrySetResult(message);
        }, cancellation);
        try
        {
            if (await Task.WhenAny(ready.Task, session.Completion).WaitAsync(cancellation) == session.Completion)
            {
                await session.Completion;
                throw new InvalidOperationException("The application stopped before it was ready.");
            }
            var announced = await ready.Task;
            return new AgentSession(RandomNumberGenerator.GetHexString(8, lowercase: true), options.Directory,
                announced["app"]?.GetValue<string>() ?? "", announced["url"]?.GetValue<string>() ?? "", session, events, run, EndedAsync(session.Completion, run));
        }
        catch
        {
            await session.DisposeAsync();
            throw;
        }
    }

    /// <summary>Ends the run as <c>isoplane dev</c> ends when its application stops: succeeded on its own, failed or crashed on an error.</summary>
    private static async Task EndedAsync(Task completion, CommandTelemetry run)
    {
        try
        {
            await completion;
            run.Ended(0);
        }
        catch (OperationCanceledException) { run.Cancelled(0); }
        catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            run.Failed(error, 1);
        }
        // The error itself reaches the agent from stop_session, which awaits the same completion.
        catch (Exception) { run.Crashed(); }
    }

    internal async Task<bool> StopAsync(string id)
    {
        if (!sessions.TryRemove(id, out var session)) return false;
        session.Run.Cancelled(0);
        await session.Session.DisposeAsync();
        await session.Ended;
        return true;
    }

    /// <inheritdoc />
    public Task StartAsync(CancellationToken cancellationToken) => Task.CompletedTask;

    /// <inheritdoc />
    public async Task StopAsync(CancellationToken cancellationToken)
    {
        foreach (var id in sessions.Keys) await StopAsync(id);
    }
}
