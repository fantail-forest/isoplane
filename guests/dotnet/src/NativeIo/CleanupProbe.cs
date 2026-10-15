using System;
using System.Net;
using System.Net.Sockets;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Leaves pending operations at a response boundary and reports later callbacks.</summary>
public static class CleanupProbe
{
    private static int _requests;
    private static int _callbacks;
    private static Socket? _idleSocket;
    private static readonly TaskCompletionSource _signal = new(TaskCreationOptions.RunContinuationsAsynchronously);

    /// <summary>Arms one abandoned operation, or observes the same guest on a later request.</summary>
    public static async Task<Response> HandleAsync(string path)
    {
        _requests++;
        if (path == "/idle-socket")
        {
            _idleSocket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
            _idleSocket.NoDelay = true;
            return Response.Text("retained");
        }
        if (path == "/read-idle-socket")
            return Response.Text($"{_requests}:{_idleSocket?.NoDelay}");
        if (path == "/promise-arm")
        {
            _ = AfterSignalAsync();
            return Response.Text("armed");
        }
        if (path == "/promise-signal")
        {
            _signal.SetResult();
            await Task.Yield();
            return Response.Text("signalled");
        }
        if (path == "/promise-complete")
        {
            Task completion = AfterSignalAsync();
            _signal.SetResult();
            await completion.ConfigureAwait(false);
            return Response.Text($"{_requests}:{_callbacks}");
        }
        if (path == "/drive-timer")
        {
            Task timer = Task.Delay(1);
            await Dns.GetHostAddressesAsync("clock-barrier.test").ConfigureAwait(false);
            await timer.ConfigureAwait(false);
            return Response.Text($"{_requests}:{_callbacks}");
        }
        if (path == "/observe") return Response.Text($"{_requests}:{_callbacks}");
        _ = AbandonAsync(path);
        if (path != "/timer")
            await Dns.GetHostAddressesAsync("barrier.test").ConfigureAwait(false);
        return Response.Text("armed");
    }

    private static async Task AfterSignalAsync()
    {
        await _signal.Task.ConfigureAwait(false);
        _callbacks++;
    }

    private static async Task AbandonAsync(string operation)
    {
        try
        {
            switch (operation)
            {
                case "/timer":
                    await Task.Delay(TimeSpan.FromDays(1)).ConfigureAwait(false);
                    break;
                case "/dns":
                    await Dns.GetHostAddressesAsync("pending.test").ConfigureAwait(false);
                    break;
                case "/socket":
                    using (var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp))
                        await socket.ConnectAsync(new IPEndPoint(IPAddress.Parse("93.184.216.34"), 80)).ConfigureAwait(false);
                    break;
                default:
                    throw new ArgumentException("Unknown cleanup operation", nameof(operation));
            }
        }
        finally
        {
            _callbacks++;
        }
    }
}
