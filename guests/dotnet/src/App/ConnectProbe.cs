using System;
using System.Globalization;
using System.Net;
using System.Net.Sockets;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Exercises an ordinary managed outbound TCP connection.</summary>
public sealed class ConnectProbe : IIsoplaneHandler
{
    /// <summary>Connects to the test endpoint and disposes the socket after resuming.</summary>
    public async Task<Response> HandleAsync(Request request, Ctx context)
    {
        string[] parts = request.Path.Split('/', StringSplitOptions.RemoveEmptyEntries);
        IPAddress address = IPAddress.Parse(parts[0]);
        int port = int.Parse(parts[1], CultureInfo.InvariantCulture);
        try
        {
            using var socket = new Socket(address.AddressFamily, SocketType.Stream, ProtocolType.Tcp);
            socket.NoDelay = true;
            if (!socket.NoDelay) throw new InvalidOperationException("Pre-connect NoDelay was not retained.");
            await socket.ConnectAsync(new IPEndPoint(address, port)).ConfigureAwait(false);
            if (!socket.NoDelay) throw new InvalidOperationException("The connected host socket did not apply NoDelay.");
            socket.NoDelay = false;
            if (socket.NoDelay) throw new InvalidOperationException("The connected host socket did not clear NoDelay.");
            if (socket.RemoteEndPoint is not IPEndPoint remote || !remote.Address.Equals(address) || remote.Port != port)
                throw new InvalidOperationException("The connected peer differs from the requested endpoint.");
            if (socket.LocalEndPoint is not IPEndPoint local || local.Port == 0)
                throw new InvalidOperationException("The connected socket has no local endpoint.");
            return Response.Text("connected");
        }
        catch (SocketException error)
        {
            return Response.Text(error.SocketErrorCode.ToString());
        }
    }
}
