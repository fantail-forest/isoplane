using System;
using System.Net;
using System.Net.Sockets;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Leaves pending operations at a response boundary and reports later callbacks.</summary>
public sealed class CleanupProbe : IIsoplaneHandler
{
    private int _requests;
    private int _callbacks;
    private Socket? _idleSocket;
    private readonly TaskCompletionSource _signal = new(TaskCreationOptions.RunContinuationsAsynchronously);

    /// <summary>Arms one abandoned operation, or observes the same guest on a later request.</summary>
    public async Task<Response> HandleAsync(Request request, Ctx context)
    {
        _requests++;
        if (request.Path == "/idle-socket")
        {
            _idleSocket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
            _idleSocket.NoDelay = true;
            return Response.Text("retained");
        }
        if (request.Path == "/read-idle-socket")
            return Response.Text($"{_requests}:{_idleSocket?.NoDelay}");
        if (request.Path == "/promise-arm")
        {
            _ = AfterSignalAsync();
            return Response.Text("armed");
        }
        if (request.Path == "/promise-signal")
        {
            _signal.SetResult();
            await Task.Yield();
            return Response.Text("signalled");
        }
        if (request.Path == "/promise-complete")
        {
            Task completion = AfterSignalAsync();
            _signal.SetResult();
            await completion.ConfigureAwait(false);
            return Response.Text($"{_requests}:{_callbacks}");
        }
        if (request.Path == "/drive-timer")
        {
            Task timer = Task.Delay(1);
            await Dns.GetHostAddressesAsync("clock-barrier.test").ConfigureAwait(false);
            await timer.ConfigureAwait(false);
            return Response.Text($"{_requests}:{_callbacks}");
        }
        if (request.Path == "/observe") return Response.Text($"{_requests}:{_callbacks}");
        _ = AbandonAsync(request.Path);
        if (request.Path != "/timer")
            await Dns.GetHostAddressesAsync("barrier.test").ConfigureAwait(false);
        return Response.Text("armed");
    }

    private async Task AfterSignalAsync()
    {
        await _signal.Task.ConfigureAwait(false);
        _callbacks++;
    }

    private async Task AbandonAsync(string operation)
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
