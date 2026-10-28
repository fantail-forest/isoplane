using System.Collections.Concurrent;
using System.Security.Cryptography;
using System.Text.Json.Nodes;

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

/// <summary>One application running for a coding agent.</summary>
internal sealed record AgentSession(string Id, string ProjectDirectory, string Application, string Url, DevelopmentSession Session, EventLog Events)
{
    internal bool Running => !Session.Completion.IsCompleted;
}

/// <summary>The sessions the agent server has started and not yet stopped; all stop when the server does.</summary>
internal sealed class AgentSessions : IAsyncDisposable
{
    private readonly ConcurrentDictionary<string, AgentSession> sessions = new(StringComparer.Ordinal);

    internal IReadOnlyCollection<AgentSession> All => sessions.Values.ToArray();

    internal AgentSession? Find(string id) => sessions.GetValueOrDefault(id);

    /// <summary>Builds and starts the project, then waits for the application to be ready.</summary>
    internal async Task<AgentSession> StartAsync(DevelopmentOptions options, CancellationToken cancellation)
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
            var started = new AgentSession(RandomNumberGenerator.GetHexString(8, lowercase: true), options.Directory,
                announced["app"]?.GetValue<string>() ?? "", announced["url"]?.GetValue<string>() ?? "", session, events);
            sessions[started.Id] = started;
            return started;
        }
        catch
        {
            await session.DisposeAsync();
            throw;
        }
    }

    internal async Task<bool> StopAsync(string id)
    {
        if (!sessions.TryRemove(id, out var session)) return false;
        await session.Session.DisposeAsync();
        return true;
    }

    public async ValueTask DisposeAsync()
    {
        foreach (var id in sessions.Keys) await StopAsync(id);
    }
}
