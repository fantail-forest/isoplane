using System.Net;
using System.Net.Sockets;
using Mono.Debugger.Soft;
using Mono.Debugging.Client;
using Mono.Debugging.Soft;

namespace Isoplane.Debugging;

internal sealed class GuestConnection(IPEndPoint endpoint, Action<Exception> failed) : SoftDebuggerStartArgs, ISoftDebuggerConnectionProvider, IAsyncDisposable
{
    private readonly Socket socket = new(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp) { LingerState = new(true, 0) };
    private Task completion = Task.CompletedTask;

    /// <summary>The session-owned connection provider.</summary>
    public override ISoftDebuggerConnectionProvider ConnectionProvider => this;

    /// <summary>Starts the handshake on a worker so editor requests remain available during guest startup.</summary>
    public IAsyncResult BeginConnect(DebuggerStartInfo info, AsyncCallback callback)
    {
        var connection = Task.Run(() => VirtualMachineManager.ConnectInternalAsync(socket, null, endpoint, null));
        completion = CompleteAsync(connection, callback);
        return connection;
    }

    private async Task CompleteAsync(Task<VirtualMachine> connection, AsyncCallback callback)
    {
        try
        {
            await connection.ContinueWith(task => callback(task), TaskScheduler.Default);
        }
        catch (Exception error) { failed(error); }
    }

    /// <summary>Returns the connected guest or propagates its connection error.</summary>
    public void EndConnect(IAsyncResult result, out VirtualMachine machine, out string appName)
    {
        machine = ((Task<VirtualMachine>)result).GetAwaiter().GetResult();
        appName = "Isoplane";
    }

    /// <summary>Closes the socket even if the guest has not completed its handshake.</summary>
    public void CancelConnect(IAsyncResult result) => Close();

    /// <summary>A closed session endpoint is terminal; it cannot be replaced by another peer.</summary>
    public bool ShouldRetryConnection(Exception error) => false;

    internal void Close() => socket.Dispose();

    /// <summary>Closes the owned socket and observes completion of the library's connection callback.</summary>
    public async ValueTask DisposeAsync()
    {
        Close();
        await completion;
    }
}
