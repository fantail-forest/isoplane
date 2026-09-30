using System.Net;
using System.Net.Sockets;
using Isoplane.Build;

namespace Isoplane.Debugging;

internal sealed class GuestSession(Action? ended = null) : IAsyncDisposable
{
    private readonly TcpClient client = new();
    private readonly CancellationTokenSource lifetime = new();

    private Task monitoring = Task.CompletedTask;
    private bool disposed;
    private readonly object gate = new();
    private Task closing = Task.CompletedTask;

    internal async Task<IPEndPoint> ConnectAsync(IPEndPoint endpoint)
    {
        await client.ConnectAsync(endpoint, lifetime.Token);
        var target = await DebugSessionProtocol.ReadEndpointAsync(client.GetStream(), lifetime.Token);
        monitoring = MonitorAsync();
        return target;
    }

    private async Task MonitorAsync()
    {
        try
        {
            byte[] unexpected = new byte[1];
            if (await client.GetStream().ReadAsync(unexpected, lifetime.Token) != 0)
                throw new IOException("The local session sent an unexpected control message.");
            if (!lifetime.IsCancellationRequested) ended?.Invoke();
        }
        catch (OperationCanceledException) when (lifetime.IsCancellationRequested)
        {
            // Cancellation is the explicit end of this session's control observation.
            return;
        }
        catch (Exception error) when (error is IOException or ObjectDisposedException)
        {
            if (lifetime.IsCancellationRequested) return;
            Console.Error.WriteLine($"isoplane: local session control closed: {error.Message}");
            ended?.Invoke();
        }
    }

    internal void Close()
    {
        lock (gate)
        {
            if (disposed) return;
            disposed = true;
            closing = CloseAsync();
        }
    }

    private async Task CloseAsync()
    {
        lifetime.Cancel();
        try
        {
            if (client.Connected)
                await DebugSessionProtocol.TerminateAsync(client.GetStream());
        }
        catch (Exception error) when (error is IOException or ObjectDisposedException)
        {
            Console.Error.WriteLine($"isoplane: closing local session control: {error.Message}");
        }
        finally { client.Dispose(); }
    }

    /// <summary>Closes the session control connection and joins its shutdown observer.</summary>
    public async ValueTask DisposeAsync()
    {
        Close();
        await closing;
        await monitoring;
        lifetime.Dispose();
    }
}
