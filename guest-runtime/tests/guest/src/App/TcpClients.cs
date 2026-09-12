using System;
using System.Globalization;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

internal static class TcpClients
{
    public static async Task<string> RunAsync(string[] path)
    {
        string mode = path[0];
        IPAddress destination = IPAddress.Parse(path[1]);
        int port = int.Parse(path[2], CultureInfo.InvariantCulture);
        AddressFamily family = mode == "only-v4" ? AddressFamily.InterNetwork :
            mode is "option-mapped" or "only-v6" ? AddressFamily.InterNetworkV6 : destination.AddressFamily;
        using var client = mode is "explicit" or "option-mapped" or "only-v6" or "only-v4"
            ? new TcpClient(family)
            : new TcpClient();
        Socket original = client.Client;
        SafeSocketHandle? exposed = mode == "exposed" ? original.SafeHandle : null;
        if (mode == "option-mapped")
        {
            original.SetSocketOption(SocketOptionLevel.IPv6, SocketOptionName.IPv6Only, 0);
            if ((int)original.GetSocketOption(SocketOptionLevel.IPv6, SocketOptionName.IPv6Only)! != 0)
                return "dual option not retained";
        }
        if (mode is "option-mapped" or "only-v6") destination = IPAddress.Loopback.MapToIPv6();
        if (mode == "only-v4")
        {
            try
            {
                await client.ConnectAsync(destination, port).ConfigureAwait(false);
                return "IPv4-only socket reached IPv6";
            }
            catch (NotSupportedException)
            {
                return "IPv4-only refused IPv6";
            }
        }
        if (mode == "only-v6")
        {
            try
            {
                await client.ConnectAsync(destination, port).ConfigureAwait(false);
                return "IPv6-only socket reached IPv4";
            }
            catch (SocketException)
            {
                return "IPv6-only refused mapped IPv4";
            }
        }
        client.NoDelay = true;
        client.ReceiveTimeout = 130;
        client.SendTimeout = 170;
        client.LingerState = new LingerOption(false, 0);
        if (mode == "cancelled")
        {
            using var cancellation = new CancellationTokenSource();
            cancellation.Cancel();
            try
            {
                await client.ConnectAsync(destination, port, cancellation.Token).ConfigureAwait(false);
                return "ignored cancellation";
            }
            catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
            {
                if (!ReferenceEquals(original, client.Client)) return "changed client";
                return "cancelled";
            }
        }
        if (mode == "dns")
        {
            await client.ConnectAsync("approved.test", port).ConfigureAwait(false);
        }
        else if (mode == "mixed")
        {
            await client.ConnectAsync(new[] { IPAddress.Loopback, destination }, port).ConfigureAwait(false);
        }
        else if (mode == "string")
        {
            await client.ConnectAsync(destination.ToString(), port).ConfigureAwait(false);
        }
        else if (mode == "list")
        {
            await client.ConnectAsync(new[] { destination }, port).ConfigureAwait(false);
        }
        else
        {
            await client.ConnectAsync(destination, port).ConfigureAwait(false);
        }
        if (mode == "connected-option")
        {
            try
            {
                original.SetSocketOption(SocketOptionLevel.IPv6, SocketOptionName.IPv6Only, 1);
                return "changed connected family mode";
            }
            catch (SocketException)
            {
                return original.DualMode ? "connected mode refused" : "changed mode despite refusal";
            }
        }
        if (!ReferenceEquals(original, client.Client)) return "changed client";
        if (exposed is not null && !ReferenceEquals(exposed, client.Client.SafeHandle)) return "changed exposed handle";
        if (!client.NoDelay || client.ReceiveTimeout != 130 || client.SendTimeout != 170 || client.LingerState.Enabled)
            return $"lost configuration: NoDelay={client.NoDelay}, receive={client.ReceiveTimeout}, send={client.SendTimeout}, linger={client.LingerState.Enabled}";
        if (original.LocalEndPoint is not IPEndPoint local || original.RemoteEndPoint is not IPEndPoint remote)
            return "connected socket endpoints missing";
        if (local.AddressFamily != original.AddressFamily || remote.AddressFamily != original.AddressFamily)
            return $"endpoint family changed: socket={original.AddressFamily}, local={local.AddressFamily}, remote={remote.AddressFamily}";
        if (!remote.Address.MapToIPv6().Equals(destination.MapToIPv6()) || remote.Port != port)
            return "connected peer identity changed";
        if (local.Port == 0) return "local endpoint port missing";
        byte[] message = { 0x77, 0x69, 0x73, 0x70 };
        using NetworkStream stream = client.GetStream();
        await stream.WriteAsync(message).ConfigureAwait(false);
        byte[] reply = new byte[message.Length];
        await stream.ReadExactlyAsync(reply).ConfigureAwait(false);
        if (!message.AsSpan().SequenceEqual(reply)) return "wrong echo";
        return "echo; same client; configuration retained";
    }
}
