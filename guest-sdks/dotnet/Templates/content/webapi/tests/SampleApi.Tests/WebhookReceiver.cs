using System.Net;
using System.Net.Sockets;
using System.Text.Json;

namespace SampleApi.Tests;

/// <summary>A loopback HTTP endpoint that records the bodies posted to it.</summary>
public sealed class WebhookReceiver : IAsyncDisposable
{
    private readonly HttpListener listener = new();
    private readonly List<string> received = [];
    private readonly Task listening;
    private TaskCompletionSource arrival = new(TaskCreationOptions.RunContinuationsAsynchronously);

    public WebhookReceiver()
    {
        Url = new Uri($"http://127.0.0.1:{FreePort()}/completed/");
        listener.Prefixes.Add(Url.ToString());
        listener.Start();
        listening = ListenAsync();
    }

    public Uri Url { get; }

    /// <summary>
    /// Waits for a posted body that <paramref name="matches"/>. The tests share one
    /// receiver, so each names the delivery it expects instead of taking the next to arrive.
    /// </summary>
    /// <exception cref="TimeoutException">No matching body arrived within <paramref name="timeout"/>.</exception>
    public async Task<T> ReceiveAsync<T>(Func<T, bool> matches, TimeSpan timeout)
    {
        using var deadline = new CancellationTokenSource(timeout);
        var examined = 0;
        while (true)
        {
            string[] bodies;
            Task next;
            lock (received)
            {
                bodies = [.. received.Skip(examined)];
                next = arrival.Task;
            }
            examined += bodies.Length;
            foreach (var body in bodies)
                if (JsonSerializer.Deserialize<T>(body, JsonSerializerOptions.Web) is { } delivery && matches(delivery))
                    return delivery;
            try { await next.WaitAsync(deadline.Token); }
            catch (OperationCanceledException)
            {
                throw new TimeoutException($"None of the {examined} webhook deliveries received within {timeout} matched.");
            }
        }
    }

    private async Task ListenAsync()
    {
        while (listener.IsListening)
        {
            HttpListenerContext context;
            try { context = await listener.GetContextAsync(); }
            catch (Exception stopped) when (stopped is HttpListenerException or ObjectDisposedException) { return; }
            using (var reader = new StreamReader(context.Request.InputStream))
                Record(await reader.ReadToEndAsync());
            context.Response.StatusCode = (int)HttpStatusCode.NoContent;
            context.Response.Close();
        }
    }

    private void Record(string body)
    {
        TaskCompletionSource arrived;
        lock (received)
        {
            received.Add(body);
            arrived = arrival;
            arrival = new(TaskCreationOptions.RunContinuationsAsynchronously);
        }
        arrived.SetResult();
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
