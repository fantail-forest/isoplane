using System.Net;
using System.Text;
using Isoplane.Testing;
using Xunit;

public sealed class ApplicationTests
{
    /// <summary>The handler fixture's launch profile caps one dispatch at this many pending operations.</summary>
    private const int PendingFuturesPerDispatch = 4;

    [Theory]
    [InlineData("Debug")]
    public async Task the_application_runs_inside_the_installed_sandbox_and_releases_its_listener(string configuration)
    {
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(20));
        var app = await IsoplaneApplication.StartAsync(
            Environment.GetEnvironmentVariable("TEST_APP")!,
            new ApplicationOptions { CliPath = Environment.GetEnvironmentVariable("TEST_CLI")!, Configuration = configuration },
            startup.Token);
        var address = app.Client.BaseAddress!;
        try
        {
            using var response = await app.Client.GetAsync("/", startup.Token);
            var body = await response.Content.ReadAsStringAsync(startup.Token);
            Assert.True(response.IsSuccessStatusCode, $"The sandbox must serve the real customer application: {body}\n{app.DiagnosticOutput}");
            Assert.True(body.Contains("Isoplane", StringComparison.Ordinal), $"The response must come from the application: {body}");
        }
        finally
        {
            await app.DisposeAsync();
        }
        using var client = new HttpClient();
        await Assert.ThrowsAsync<HttpRequestException>(() => client.GetAsync(address, startup.Token));
    }

    [Fact]
    public async Task operations_past_the_dispatch_cap_complete_in_start_order_without_failing()
    {
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(20));
        const int count = PendingFuturesPerDispatch * 3;
        int port = FreePort();
        using var peer = new HttpListener();
        peer.Prefixes.Add($"http://127.0.0.1:{port}/");
        peer.Start();
        var serving = ServeIndexesAsync(peer, count, startup.Token);
        await using var app = await IsoplaneApplication.StartAsync(
            Environment.GetEnvironmentVariable("TEST_APP")!,
            new ApplicationOptions { CliPath = Environment.GetEnvironmentVariable("TEST_CLI")! },
            startup.Token);
        using var response = await app.Client.GetAsync($"/fan-out?count={count}&port={port}", startup.Token);
        var body = await response.Content.ReadAsStringAsync(startup.Token);
        Assert.True(response.IsSuccessStatusCode, $"every operation past the cap must complete: {body}\n{app.DiagnosticOutput}");
        Assert.Equal(string.Join(",", Enumerable.Range(0, count)), body);
        Assert.Equal(count, await serving);
        peer.Stop();
    }

    /// <summary>Answers each request with the index in its path until <paramref name="count"/> have been served.</summary>
    private static async Task<int> ServeIndexesAsync(HttpListener peer, int count, CancellationToken cancellation)
    {
        int served = 0;
        while (served < count)
        {
            var context = await peer.GetContextAsync().WaitAsync(cancellation);
            byte[] index = Encoding.UTF8.GetBytes(context.Request.Url!.AbsolutePath.TrimStart('/'));
            context.Response.ContentLength64 = index.Length;
            await context.Response.OutputStream.WriteAsync(index, cancellation);
            context.Response.Close();
            served++;
        }
        return served;
    }

    private static int FreePort()
    {
        using var socket = new System.Net.Sockets.TcpListener(IPAddress.Loopback, 0);
        socket.Start();
        return ((IPEndPoint)socket.LocalEndpoint).Port;
    }
}
