using System;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Exercises ordinary asynchronous NetworkStream reads and writes.</summary>
public static class StreamProbe
{
    /// <summary>Reads the endpoint's payload and writes the same bytes back.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request request)
    {
        string[] parts = request.Path.Split('/', StringSplitOptions.RemoveEmptyEntries);
        IPAddress address = IPAddress.Parse(parts[0]);
        int port = int.Parse(parts[1], CultureInfo.InvariantCulture);
        using var socket = new Socket(address.AddressFamily, SocketType.Stream, ProtocolType.Tcp);
        await socket.ConnectAsync(new IPEndPoint(address, port)).ConfigureAwait(false);
        using var stream = new NetworkStream(socket);
        string mode = parts.Length > 2 ? parts[2] : "transfer";
        if (mode == "half") socket.Shutdown(SocketShutdown.Send);
        using var cancellation = new CancellationTokenSource();
        if (mode == "cancel") cancellation.CancelAfter(100);
        byte[] bytes = new byte[4];
        try
        {
            await stream.ReadExactlyAsync(bytes, cancellation.Token).ConfigureAwait(false);
            if (mode == "half") return Response.Text(Convert.ToHexString(bytes));
            await stream.WriteAsync(bytes).ConfigureAwait(false);
            return Response.Text("transferred");
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            return Response.Text("cancelled");
        }
        catch (EndOfStreamException)
        {
            return Response.Text("eof");
        }
        catch (IOException)
        {
            return Response.Text("stream-error");
        }
    }
}
