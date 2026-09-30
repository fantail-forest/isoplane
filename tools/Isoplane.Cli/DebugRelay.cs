using System.Net;
using System.Net.Sockets;
using Isoplane.Build;

namespace Isoplane.Cli;

internal sealed class DebugRelay : IAsyncDisposable
{
    private readonly TcpListener listener = new(IPAddress.Loopback, 0);
    private readonly TcpListener adapterListener = new(IPAddress.Loopback, 0);
    private readonly TcpListener editorListener = new(IPAddress.Loopback, 0);
    private readonly CancellationTokenSource lifetime;
    private readonly Task forwarding;

    internal IPEndPoint Endpoint { get; }
    internal IPEndPoint AdapterEndpoint { get; }
    internal Task Completion => forwarding;

    internal DebugRelay(CancellationToken cancellation)
    {
        lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        listener.Start();
        Endpoint = (IPEndPoint)listener.LocalEndpoint;
        adapterListener.Start();
        editorListener.Start();
        AdapterEndpoint = (IPEndPoint)editorListener.LocalEndpoint;
        forwarding = ForwardAsync(lifetime.Token);
    }

    private async Task ForwardAsync(CancellationToken cancellation)
    {
        try
        {
            using var editor = await editorListener.AcceptTcpClientAsync(cancellation);
            await DebugSessionProtocol.WriteEndpointAsync(editor.GetStream(), (IPEndPoint)adapterListener.LocalEndpoint, cancellation);
            using var connected = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            var pairs = ForwardGuestsAsync(connected.Token);
            var control = DebugSessionProtocol.ReadTerminationAsync(editor.GetStream(), connected.Token);
            await Task.WhenAny(pairs, control);
            connected.Cancel();
            try { await Task.WhenAll(pairs, control); }
            catch (OperationCanceledException) when (connected.IsCancellationRequested)
            {
                cancellation.ThrowIfCancellationRequested();
            }
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            Console.Error.WriteLine("isoplane: debugger relay stopped.");
        }
        catch (IOException error)
        {
            throw new IOException($"Debugger transport failed: {error.Message}", error);
        }
        finally
        {
            listener.Stop();
            adapterListener.Stop();
            editorListener.Stop();
        }
    }

    private async Task ForwardGuestsAsync(CancellationToken cancellation)
    {
        while (true)
        {
            using var adapter = await adapterListener.AcceptTcpClientAsync(cancellation);
            using var guest = await listener.AcceptTcpClientAsync(cancellation);
            try { await ForwardPairAsync(adapter, guest, cancellation); }
            catch (IOException error)
            {
                Console.Error.WriteLine($"isoplane: guest debugger transport closed: {error.Message}");
            }
        }
    }

    private static async Task ForwardPairAsync(TcpClient adapter, TcpClient guest, CancellationToken cancellation)
    {
        using var connection = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        var outbound = adapter.GetStream().CopyToAsync(guest.GetStream(), connection.Token);
        var inbound = guest.GetStream().CopyToAsync(adapter.GetStream(), connection.Token);
        await Task.WhenAny(outbound, inbound);
        connection.Cancel();
        try { await Task.WhenAll(outbound, inbound); }
        catch (OperationCanceledException) when (connection.IsCancellationRequested)
        {
            cancellation.ThrowIfCancellationRequested();
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
