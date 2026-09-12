using System;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.WebSockets;
using System.Text;
using System.Security.Cryptography.X509Certificates;
using System.Threading;
using System.Threading.Tasks;

namespace Acceptance;

internal static class Protocols
{
    public static async Task<string> RunAsync(string[] path)
    {
        string mode = path[0];
        int port = int.Parse(path[1], CultureInfo.InvariantCulture);
        if (mode.StartsWith("ws-", StringComparison.Ordinal))
            return await WebSocketAsync(mode, port).ConfigureAwait(false);

        var origin = new Uri($"http://127.0.0.1:{port}");
        bool proxy = mode != "auth";
        bool tunnel = mode is "proxy-tls" or "proxy-tls-auth";
        using var handler = new SocketsHttpHandler { UseProxy = proxy };
        if (proxy)
        {
            handler.Proxy = new WebProxy(origin)
            {
                Credentials = mode is "proxy-auth" or "proxy-tls-auth" ? new NetworkCredential("user", "pass") : null
            };
        }
        else
        {
            handler.Credentials = new NetworkCredential("user", "pass");
        }
        if (tunnel) TrustFixtureRoot(handler);
        using var client = new HttpClient(handler);
        var target = tunnel
            ? new Uri($"https://leaf.acceptance.test:{path[2]}/through-tunnel")
            : proxy ? new Uri($"http://example.invalid/{mode}") : new Uri(origin, "/auth");
        using var response = await client.GetAsync(target).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        return await response.Content.ReadAsStringAsync().ConfigureAwait(false);
    }

    private static async Task<string> WebSocketAsync(string mode, int port)
    {
        using var socket = new ClientWebSocket();
        if (mode != "ws-default") socket.Options.KeepAliveInterval = TimeSpan.Zero;
        bool secure = mode == "ws-secure";
        var destination = new Uri(secure ? $"wss://leaf.acceptance.test:{port}/{mode}" : $"ws://127.0.0.1:{port}/{mode}");
        using var handler = new SocketsHttpHandler { UseProxy = false };
        if (secure) TrustFixtureRoot(handler);
        using var invoker = new HttpMessageInvoker(handler);
        if (mode == "ws-bad")
        {
            try
            {
                await socket.ConnectAsync(destination, invoker, CancellationToken.None).ConfigureAwait(false);
                return "accepted non-upgrade";
            }
            catch (WebSocketException)
            {
                return "refused non-upgrade";
            }
        }
        await socket.ConnectAsync(destination, invoker, CancellationToken.None).ConfigureAwait(false);
        if (mode == "ws-cancel")
        {
            using var cancellation = new CancellationTokenSource();
            Task<WebSocketReceiveResult> pending = socket.ReceiveAsync(new ArraySegment<byte>(new byte[8]), cancellation.Token);
            if (pending.IsCompleted) return "receive did not wait";
            cancellation.Cancel();
            try
            {
                await pending.ConfigureAwait(false);
                return "receive ignored cancellation";
            }
            catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
            {
                return "receive cancelled";
            }
        }
        byte[] message = Encoding.UTF8.GetBytes("sandbox echo");
        WebSocketMessageType kind = mode == "ws-binary" ? WebSocketMessageType.Binary : WebSocketMessageType.Text;
        await socket.SendAsync(message.AsMemory(0, 3), kind, false, CancellationToken.None).ConfigureAwait(false);
        await socket.SendAsync(message.AsMemory(3), kind, true, CancellationToken.None).ConfigureAwait(false);
        using var received = new MemoryStream();
        byte[] buffer = new byte[3];
        while (true)
        {
            ValueWebSocketReceiveResult result = await socket.ReceiveAsync(buffer.AsMemory(), CancellationToken.None).ConfigureAwait(false);
            if (result.MessageType != kind) return "wrong message type";
            received.Write(buffer, 0, result.Count);
            if (result.EndOfMessage) break;
        }
        if (!message.AsSpan().SequenceEqual(received.ToArray())) return "wrong message bytes";
        await socket.CloseAsync(WebSocketCloseStatus.NormalClosure, "done", CancellationToken.None).ConfigureAwait(false);
        return socket.State == WebSocketState.Closed ? "echo and close" : "close incomplete";
    }

    private static void TrustFixtureRoot(SocketsHttpHandler handler)
    {
        var policy = new X509ChainPolicy { TrustMode = X509ChainTrustMode.CustomRootTrust, RevocationMode = X509RevocationMode.NoCheck };
        policy.CustomTrustStore.Add(X509Certificate2.CreateFromPem(CryptoMaterial.RootCertificate));
        handler.SslOptions.CertificateChainPolicy = policy;
    }

}
