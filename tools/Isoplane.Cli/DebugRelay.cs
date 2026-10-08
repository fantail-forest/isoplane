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
    private readonly string package;
    private readonly DebugProject? project;
    private readonly SemaphoreSlim writes = new(1, 1);
    private volatile Stream? editorControl;

    internal IPEndPoint Endpoint { get; }
    internal IPEndPoint AdapterEndpoint { get; }
    private readonly TaskCompletionSource editorConnected = new(TaskCreationOptions.RunContinuationsAsynchronously);
    internal Task EditorConnected => editorConnected.Task;
    internal Task Completion => forwarding;
    internal bool Detached { get; private set; }
    internal Func<CancellationToken, Task>? PrepareDetach { get; set; }

    /// <param name="package">The application package the session's guests run, which the debugger reads symbols from.</param>
    internal DebugRelay(string package, CancellationToken cancellation, DebugProject? project = null)
    {
        this.package = package;
        this.project = project;
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
            editorConnected.TrySetResult();
            // Held across the opening message, so a rebuild notice can only follow it.
            await writes.WaitAsync(cancellation);
            try
            {
                editorControl = editor.GetStream();
                await DebugSessionProtocol.WriteEndpointAsync(editor.GetStream(), (IPEndPoint)adapterListener.LocalEndpoint, package, project, cancellation);
            }
            finally { writes.Release(); }
            using var connected = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            var pairs = ForwardGuestsAsync(connected.Token);
            var control = DebugSessionProtocol.ReadTerminationAsync(editor.GetStream(), writes, token => PrepareDetach is { } prepare ? prepare(token) : throw new InvalidOperationException("The local host is not ready to detach."), connected.Token);
            if (await Task.WhenAny(pairs, control) == control) Detached = await control;
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
            editorControl = null;
            listener.Stop();
            adapterListener.Stop();
            editorListener.Stop();
        }
    }

    /// <summary>Tells a connected debugger that the application package was rebuilt, so it reads the new symbols.</summary>
    internal async Task PackageChangedAsync(CancellationToken cancellation)
    {
        if (editorControl is not { } control) return;
        try { await DebugSessionProtocol.ReplyAsync(control, writes, DebugSessionProtocol.PackageChanged, null, cancellation); }
        catch (Exception error) when (error is IOException or ObjectDisposedException)
        {
            // The debugger went away while this was written; the control reader ends the session for that.
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
        adapter.NoDelay = true;
        guest.NoDelay = true;
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
        finally
        {
            lifetime.Dispose();
            writes.Dispose();
        }
    }
}
