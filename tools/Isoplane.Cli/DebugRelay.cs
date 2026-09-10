using System.Net;
using System.Net.Sockets;

namespace Wisp.Cli;

internal sealed class DebugRelay : IAsyncDisposable
{
    private readonly TcpListener listener = new(IPAddress.Loopback, 0);
    private readonly CancellationTokenSource lifetime;
    private readonly Task forwarding;

    internal IPEndPoint Endpoint { get; }
    internal Task Completion => forwarding;

    internal DebugRelay(CancellationToken cancellation)
    {
        lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        listener.Start();
        Endpoint = (IPEndPoint)listener.LocalEndpoint;
        forwarding = ForwardAsync(lifetime.Token);
    }

    private async Task ForwardAsync(CancellationToken cancellation)
    {
        try
        {
            using var first = await listener.AcceptTcpClientAsync(cancellation);
            using var second = await listener.AcceptTcpClientAsync(cancellation);
            listener.Stop();
            using var connection = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            var outbound = first.GetStream().CopyToAsync(second.GetStream(), connection.Token);
            var inbound = second.GetStream().CopyToAsync(first.GetStream(), connection.Token);
            await Task.WhenAny(outbound, inbound);
            connection.Cancel();
            try { await Task.WhenAll(outbound, inbound); }
            catch (OperationCanceledException) when (connection.IsCancellationRequested)
            {
                cancellation.ThrowIfCancellationRequested();
            }
            if (!cancellation.IsCancellationRequested)
                Console.Error.WriteLine("isoplane: debugger transport closed.");
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            Console.Error.WriteLine("isoplane: debugger relay stopped.");
        }
        catch (IOException error)
        {
            Console.Error.WriteLine($"isoplane: debugger transport ended: {error.Message}.");
        }
        finally
        {
            listener.Stop();
        }
    }

    /// <summary>Closes both debugger peers and waits for owned forwarding to stop.</summary>
    public async ValueTask DisposeAsync()
    {
        lifetime.Cancel();
        try { await forwarding; }
        finally { lifetime.Dispose(); }
    }
}
