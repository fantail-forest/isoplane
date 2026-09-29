using System;
using System.Net;
using System.Net.Sockets;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Exercises managed exceptions for forbidden inbound networking.</summary>
public sealed class NetworkRefusalProbe : IIsoplaneHandler
{
    /// <summary>Returns the actual SocketError raised by an attempted listener.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        try
        {
            switch (request.Path)
            {
                case "/tcp-listener":
                    using (var listener = new TcpListener(IPAddress.Loopback, 0)) listener.Start();
                    break;
                case "/tcp-bind":
                    using (var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp))
                        socket.Bind(new IPEndPoint(IPAddress.Loopback, 0));
                    break;
                case "/tcp-listen":
                    using (var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp))
                        socket.Listen(1);
                    break;
                case "/keepalive":
                    using (var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp))
                        socket.SetSocketOption(SocketOptionLevel.Socket, SocketOptionName.KeepAlive, true);
                    break;
                case "/keepalive-count":
                    using (var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp))
                        socket.SetSocketOption(SocketOptionLevel.Tcp, SocketOptionName.TcpKeepAliveRetryCount, 3);
                    break;
                case "/receive-buffer":
                    using (var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp))
                        return Task.FromResult(Response.Text(socket.ReceiveBufferSize.ToString()));
                case "/udp-client":
                    using (var client = new UdpClient(0)) { }
                    break;
                case "/udp-bind":
                    using (var socket = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp))
                        socket.Bind(new IPEndPoint(IPAddress.Loopback, 0));
                    break;
                default:
                    throw new ArgumentException("Unknown network refusal case.");
            }
            return Task.FromResult(Response.Text("unexpected success"));
        }
        catch (SocketException error)
        {
            return Task.FromResult(Response.Text(error.SocketErrorCode.ToString()));
        }
    }
}
