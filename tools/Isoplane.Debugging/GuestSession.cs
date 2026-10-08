using System.Net;
using System.Net.Sockets;
using Isoplane.Build;

namespace Isoplane.Debugging;

internal sealed class GuestSession(Action? ended = null, Action? packageChanged = null) : IAsyncDisposable
{
    internal DebugProject? Project { get; private set; }
    private readonly TcpClient client = new();
    private readonly CancellationTokenSource lifetime = new();

    private readonly SemaphoreSlim writes = new(1, 1);
    private TaskCompletionSource? prepared;
    private TaskCompletionSource? committed;
    private bool detached;

    private Task monitoring = Task.CompletedTask;
    private bool disposed;
    private readonly object gate = new();
    private Task closing = Task.CompletedTask;

    /// <summary>Connects to the local session at <paramref name="endpoint"/>. Returns where its guests' debugger agents are reached and the application package they run.</summary>
    internal async Task<(IPEndPoint Guests, string Package)> ConnectAsync(IPEndPoint endpoint)
    {
        await client.ConnectAsync(endpoint, lifetime.Token);
        var target = await DebugSessionProtocol.ReadEndpointAsync(client.GetStream(), lifetime.Token);
        Project = target.Project;
        monitoring = MonitorAsync();
        return (target.Guests, target.Package);
    }

    private async Task MonitorAsync()
    {
        try
        {
            while (await DebugSessionProtocol.ReadReplyAsync(client.GetStream(), lifetime.Token) is { } reply)
            {
                if (reply.Action == DebugSessionProtocol.PackageChanged)
                {
                    packageChanged?.Invoke();
                    continue;
                }
                var pending = reply.Action switch
                {
                    "detachReady" => prepared,
                    "detached" => committed,
                    _ => throw new IOException("The local session sent an unexpected control message.")
                } ?? throw new IOException("The local session replied without a matching detach request.");
                if (reply.Error is { } error) pending.TrySetException(new IOException(error));
                else
                {
                    if (reply.Action == "detached") detached = true;
                    pending.TrySetResult();
                }
            }
            if (!lifetime.IsCancellationRequested && !detached) ended?.Invoke();
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
        finally
        {
            prepared?.TrySetException(new IOException("The local session closed during detach preparation."));
            committed?.TrySetException(new IOException("The local session closed before confirming detachment."));
        }
    }

    internal async Task PrepareDetachAsync()
    {
        prepared = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await WriteAsync(stream => DebugSessionProtocol.RequestDetachAsync(stream, false, lifetime.Token));
        await prepared.Task;
    }

    internal async Task DetachAsync()
    {
        committed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await WriteAsync(stream => DebugSessionProtocol.RequestDetachAsync(stream, true, lifetime.Token));
        await committed.Task;
    }

    private async Task WriteAsync(Func<Stream, Task> write)
    {
        await writes.WaitAsync();
        try { await write(client.GetStream()); }
        finally { writes.Release(); }
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
            if (client.Connected && !detached)
                await WriteAsync(DebugSessionProtocol.TerminateAsync);
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
        writes.Dispose();
    }
}
