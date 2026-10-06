using System;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Exercises the ordinary managed asynchronous DNS API.</summary>
public static class DnsProbe
{
    /// <summary>Returns the first host-resolved address.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request request, Ctx context)
    {
        using var cancellation = new CancellationTokenSource();
        if (request.Path == "/cancel.test") cancellation.CancelAfter(100);
        try
        {
            IPAddress[] addresses = await Dns.GetHostAddressesAsync(request.Path.Substring(1), cancellation.Token).ConfigureAwait(false);
            return Response.Text(addresses[0].ToString());
        }
        catch (SocketException error)
        {
            return Response.Text(error.SocketErrorCode.ToString());
        }
        catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
        {
            return Response.Text("cancelled");
        }
    }
}
