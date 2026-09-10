using System;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// Outbound TCP through the source-built socket engine: readiness comes from
/// the pollables libc registers for a socket, a failed connection is an
/// inline error, and every state the peer can put the connection in reaches
/// the ordinary managed exception.
/// </summary>
internal sealed class Sockets
{
    public async Task<string> RunAsync(string[] path)
    {
        switch (path[0])
        {
            case "connect":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                return "connected " + socket.LocalEndPoint + "->" + socket.RemoteEndPoint;
            }
            case "echo":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                return await EchoAsync(socket, Convert.FromHexString(path[3])).ConfigureAwait(false);
            }
            case "two":
            {
                // Two sockets in flight at once: each hook must observe its own pollables.
                using var first = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var second = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                Task<string> a = EchoAsync(first, Convert.FromHexString(path[3]));
                Task<string> b = EchoAsync(second, Convert.FromHexString(path[4]));
                await Task.WhenAll(a, b).ConfigureAwait(false);
                return a.Result + "+" + b.Result;
            }
            case "refused":
                return await ExpectSocketError(() => ConnectAsync(path[1], path[2])).ConfigureAwait(false);
            case "eof":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                int count = await socket.ReceiveAsync(new byte[4], SocketFlags.None).ConfigureAwait(false);
                return count == 0 ? "eof" : "received " + count.ToString(CultureInfo.InvariantCulture);
            }
            case "reset":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                return await ExpectSocketError(() => socket.ReceiveAsync(new byte[4], SocketFlags.None)).ConfigureAwait(false);
            }
            case "half":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                // Shutdown marks the socket disconnected for NetworkStream's
                // constructor on every platform; the socket itself still receives.
                socket.Shutdown(SocketShutdown.Send);
                byte[] bytes = new byte[4];
                int received = 0;
                while (received < bytes.Length)
                {
                    int count = await socket.ReceiveAsync(new Memory<byte>(bytes, received, bytes.Length - received), SocketFlags.None).ConfigureAwait(false);
                    if (count == 0)
                    {
                        return "eof after " + received.ToString(CultureInfo.InvariantCulture);
                    }
                    received += count;
                }
                return Convert.ToHexString(bytes);
            }
            case "cancel":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                using var cancellation = new CancellationTokenSource();
                cancellation.CancelAfter(int.Parse(path[3], CultureInfo.InvariantCulture));
                try
                {
                    await socket.ReceiveAsync(new byte[4], SocketFlags.None, cancellation.Token).ConfigureAwait(false);
                    return "received";
                }
                catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
                {
                    return "cancelled";
                }
            }
            case "dispose-then-connect":
            {
                // A disposed socket's registration must be gone before the next poll.
                var first = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                first.Dispose();
                using var second = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                return await EchoAsync(second, Convert.FromHexString(path[3])).ConfigureAwait(false);
            }
            case "nodelay":
            {
                using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
                socket.NoDelay = true;
                bool before = socket.NoDelay;
                await socket.ConnectAsync(Endpoint(path[1], path[2])).ConfigureAwait(false);
                bool connected = socket.NoDelay;
                socket.NoDelay = false;
                return before + " " + connected + " " + socket.NoDelay;
            }
            case "timeouts":
            {
                // Timeouts govern blocking calls, which never block here; they
                // are stored like any other option, as for asynchronous use elsewhere.
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                socket.ReceiveTimeout = 1500;
                socket.SendTimeout = 2500;
                string socketTimeouts = socket.ReceiveTimeout + " " + socket.SendTimeout;
                using var client = new TcpClient();
                client.ReceiveTimeout = 3500;
                // A stream's timeouts are its socket's, as on every platform.
                var stream = new NetworkStream(socket, ownsSocket: false);
                stream.ReadTimeout = 4500;
                stream.WriteTimeout = 5500;
                return socketTimeouts + " " + client.ReceiveTimeout + " " + stream.ReadTimeout + " " + stream.WriteTimeout;
            }
            case "reset-send":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                // Let the peer's reset arrive before writing into it.
                await Task.Delay(int.Parse(path[3], CultureInfo.InvariantCulture)).ConfigureAwait(false);
                return await ExpectSocketError(async () =>
                {
                    byte[] payload = new byte[4096];
                    for (int attempt = 0; attempt < 64; attempt++)
                    {
                        await socket.SendAsync(payload, SocketFlags.None).ConfigureAwait(false);
                    }
                }).ConfigureAwait(false);
            }
            case "sync-connect":
            {
                // Sockets never block here; the synchronous call reports it.
                using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
                return await ExpectSocketError(() => { socket.Connect(Endpoint(path[1], path[2])); return Task.CompletedTask; }).ConfigureAwait(false);
            }
            case "sync-receive":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                return await ExpectSocketError(() => { socket.Receive(new byte[4]); return Task.CompletedTask; }).ConfigureAwait(false);
            }
            case "sync-send":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                int sent = socket.Send(new byte[] { 1, 2, 3, 4 });
                return "sent " + sent.ToString(CultureInfo.InvariantCulture);
            }
            case "sync-poll":
            {
                using var socket = await ConnectAsync(path[1], path[2]).ConfigureAwait(false);
                try
                {
                    return "polled " + socket.Poll(100_000, SelectMode.SelectRead);
                }
                catch (Exception error)
                {
                    return error.GetType().Name;
                }
            }
            case "listen":
                return await ExpectSocketError(() =>
                {
                    using var listener = new TcpListener(IPAddress.Loopback, 0);
                    listener.Start();
                    return Task.CompletedTask;
                }).ConfigureAwait(false);
            case "udp":
                return await ExpectSocketError(() =>
                {
                    using var client = new UdpClient(AddressFamily.InterNetwork);
                    return Task.CompletedTask;
                }).ConfigureAwait(false);
            case "receive-timeout":
            {
                using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
                try
                {
                    socket.ReceiveTimeout = 250;
                    return "timeout " + socket.ReceiveTimeout.ToString(CultureInfo.InvariantCulture);
                }
                catch (Exception error)
                {
                    return error.GetType().Name;
                }
            }
            default:
                throw new ArgumentException("unknown sockets case " + path[0]);
        }
    }

    private static IPEndPoint Endpoint(string address, string port) =>
        new(IPAddress.Parse(address), int.Parse(port, CultureInfo.InvariantCulture));

    private static async Task<Socket> ConnectAsync(string address, string port)
    {
        IPEndPoint endpoint = Endpoint(address, port);
        var socket = new Socket(endpoint.AddressFamily, SocketType.Stream, ProtocolType.Tcp);
        try
        {
            await socket.ConnectAsync(endpoint).ConfigureAwait(false);
            return socket;
        }
        catch
        {
            socket.Dispose();
            throw;
        }
    }

    private static async Task<string> EchoAsync(Socket socket, byte[] payload)
    {
        using var stream = new NetworkStream(socket);
        await stream.WriteAsync(payload).ConfigureAwait(false);
        byte[] echoed = new byte[payload.Length];
        await stream.ReadExactlyAsync(echoed).ConfigureAwait(false);
        return Convert.ToHexString(echoed);
    }

    private static async Task<string> ExpectSocketError(Func<Task> operation)
    {
        try
        {
            await operation().ConfigureAwait(false);
            return "no error";
        }
        catch (SocketException error)
        {
            // An unmapped errno is the diagnosis when the code is the generic one.
            return error.SocketErrorCode == SocketError.SocketError
                ? "SocketError(errno " + error.NativeErrorCode.ToString(CultureInfo.InvariantCulture) + ")"
                : error.SocketErrorCode.ToString();
        }
        catch (IOException error) when (error.InnerException is SocketException inner)
        {
            return inner.SocketErrorCode.ToString();
        }
    }
}
