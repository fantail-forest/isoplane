using System.Net.Sockets;
using System.Text.Json;
using Isoplane.Native;

namespace Isoplane.Cli;

/// <summary>
/// What an application started in the editor's terminal tells the debug adapter that waits for it: where the
/// adapter reaches its debugger, and the application that is ready.
/// </summary>
internal sealed record DebugLaunchAnnouncement(string Endpoint, string App, string Url)
{
    private static readonly JsonSerializerOptions Wire = new(JsonSerializerDefaults.Web) { RespectRequiredConstructorParameters = true, RespectNullableAnnotations = true };

    /// <summary>The announcement as the one line it is sent in.</summary>
    internal string Line => JsonSerializer.Serialize(this, Wire);

    /// <summary>Reads the line <see cref="Line"/> wrote. Throws <see cref="JsonException"/> when it is anything else.</summary>
    internal static DebugLaunchAnnouncement Read(string line) =>
        JsonSerializer.Deserialize<DebugLaunchAnnouncement>(line, Wire) ?? throw new JsonException("The announcement is an object.");
}

internal sealed class TerminalDebugLaunch(TcpClient client, Task connected, CancellationTokenSource lifetime) : IAsyncDisposable
{
    private readonly Task monitoring = MonitorAsync(client, connected, lifetime);

    internal async Task AnnounceAsync(string endpoint, SessionEvent.Ready ready, CancellationToken cancellation)
    {
        using (var writer = new StreamWriter(client.GetStream(), leaveOpen: true))
            await writer.WriteLineAsync(new DebugLaunchAnnouncement(endpoint, ready.App, ready.Url).Line.AsMemory(), cancellation);
        await connected.WaitAsync(cancellation);
        await monitoring;
        client.Dispose();
    }

    private static async Task MonitorAsync(TcpClient client, Task connected, CancellationTokenSource lifetime)
    {
        using var reading = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
        try
        {
            var closed = client.GetStream().ReadAsync(new byte[1], reading.Token).AsTask();
            if (await Task.WhenAny(closed, connected) == closed)
            {
                await closed;
                if (!connected.IsCompletedSuccessfully) lifetime.Cancel();
            }
            reading.Cancel();
            try { await closed; }
            catch (OperationCanceledException) when (reading.IsCancellationRequested) { }
        }
        catch (IOException) when (connected.IsCompletedSuccessfully)
        {
            // The debugger control connection owns shutdown after the startup handoff.
        }
        catch (IOException error)
        {
            Console.Error.WriteLine($"isoplane: editor startup connection closed: {error.Message}");
            lifetime.Cancel();
        }
        catch (OperationCanceledException) when (lifetime.IsCancellationRequested) { }
    }

    /// <summary>Joins startup observation and releases its discovery connection.</summary>
    public async ValueTask DisposeAsync()
    {
        try { await monitoring; }
        finally { client.Dispose(); }
    }
}
