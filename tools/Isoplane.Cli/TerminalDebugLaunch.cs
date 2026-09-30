using System.Net.Sockets;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Isoplane.Cli;

internal sealed class TerminalDebugLaunch(TcpClient client, Task connected, CancellationTokenSource lifetime) : IAsyncDisposable
{
    private readonly Task monitoring = MonitorAsync(client, connected, lifetime);

    internal async Task AnnounceAsync(string endpoint, JsonObject ready, CancellationToken cancellation)
    {
        using (var writer = new StreamWriter(client.GetStream(), leaveOpen: true))
            await writer.WriteLineAsync(JsonSerializer.Serialize(new { endpoint, app = ready["app"]?.GetValue<string>(), url = ready["url"]?.GetValue<string>() }).AsMemory(), cancellation);
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
