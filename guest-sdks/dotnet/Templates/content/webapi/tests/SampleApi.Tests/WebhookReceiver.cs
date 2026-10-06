using System.Net;
using System.Net.Sockets;
using System.Threading.Channels;

namespace SampleApi.Tests;

/// <summary>A loopback HTTP endpoint that records the bodies posted to it.</summary>
public sealed class WebhookReceiver : IAsyncDisposable
{
    private readonly HttpListener listener = new();
    private readonly Channel<string> received = Channel.CreateUnbounded<string>();
    private readonly Task listening;

    public WebhookReceiver()
    {
        Url = new Uri($"http://127.0.0.1:{FreePort()}/completed/");
        listener.Prefixes.Add(Url.ToString());
        listener.Start();
        listening = ListenAsync();
    }

    public Uri Url { get; }

    /// <summary>Waits for the next posted body.</summary>
    public async Task<string> NextAsync(TimeSpan timeout)
    {
        using var deadline = new CancellationTokenSource(timeout);
        return await received.Reader.ReadAsync(deadline.Token);
    }

    private async Task ListenAsync()
    {
        while (listener.IsListening)
        {
            HttpListenerContext context;
            try { context = await listener.GetContextAsync(); }
            catch (Exception stopped) when (stopped is HttpListenerException or ObjectDisposedException) { return; }
            using (var body = new StreamReader(context.Request.InputStream))
                received.Writer.TryWrite(await body.ReadToEndAsync());
            context.Response.StatusCode = (int)HttpStatusCode.NoContent;
            context.Response.Close();
        }
    }

    private static int FreePort()
    {
        using var probe = new TcpListener(IPAddress.Loopback, 0);
        probe.Start();
        return ((IPEndPoint)probe.LocalEndpoint).Port;
    }

    public async ValueTask DisposeAsync()
    {
        listener.Close();
        await listening;
    }
}
