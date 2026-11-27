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

    [Fact]
    public async Task content_a_test_supplies_is_served_beside_the_projects_and_in_place_of_a_file_at_the_same_path()
    {
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(20));
        var project = Environment.GetEnvironmentVariable("TEST_APP")!;
        var supplied = Directory.CreateTempSubdirectory("testing-consumer-content-");
        try
        {
            // The handler fixture serves the content file at /content/NAME and bundles kept.txt and replaced.txt itself.
            byte[] added = RandomBytes(), replacement = RandomBytes(), single = RandomBytes(), later = RandomBytes(), second = RandomBytes();
            Directory.CreateDirectory(Path.Combine(supplied.FullName, "tree", "content", "nested"));
            Directory.CreateDirectory(Path.Combine(supplied.FullName, "other", "content", "other"));
            File.WriteAllBytes(Path.Combine(supplied.FullName, "tree", "content", "nested", "added.bin"), added);
            File.WriteAllBytes(Path.Combine(supplied.FullName, "tree", "content", "replaced.txt"), replacement);
            File.WriteAllBytes(Path.Combine(supplied.FullName, "tree", "content", "twice.bin"), RandomBytes());
            File.WriteAllBytes(Path.Combine(supplied.FullName, "other", "content", "other", "second.bin"), second);
            File.WriteAllBytes(Path.Combine(supplied.FullName, "single.bin"), single);
            File.WriteAllBytes(Path.Combine(supplied.FullName, "later.bin"), later);
            var projectContent = ContentOf(Path.Combine(project, "content"));
            var app = await IsoplaneApplication.StartAsync(
                project,
                new ApplicationOptions
                {
                    CliPath = Environment.GetEnvironmentVariable("TEST_CLI")!,
                    Content =
                    [
                        new ContentDirectory(Path.Combine(supplied.FullName, "tree")),
                        new ContentDirectory(Path.Combine(supplied.FullName, "other")),
                        new ContentFile("content/single.bin", Path.Combine(supplied.FullName, "single.bin")),
                        new ContentFile("content/twice.bin", Path.Combine(supplied.FullName, "later.bin"))
                    ]
                },
                startup.Token);
            await using (app)
            {
                async Task AssertServedAsync(string name, byte[] expected, string because)
                {
                    using var response = await app.Client.GetAsync("/content/" + name, startup.Token);
                    Assert.True(response.StatusCode == HttpStatusCode.OK, $"{because}: {name} answered {(int)response.StatusCode}\n{app.DiagnosticOutput}");
                    var served = await response.Content.ReadAsByteArrayAsync(startup.Token);
                    Assert.True(expected.SequenceEqual(served), $"{because}: {name} did not carry the expected bytes");
                }
                await AssertServedAsync("nested/added.bin", added, "a file in a supplied directory must be served at its path beneath that directory");
                await AssertServedAsync("other/second.bin", second, "a file in a second supplied directory must be served beside the first directory's");
                await AssertServedAsync("single.bin", single, "a supplied file must be served at the path the test gave it");
                await AssertServedAsync("replaced.txt", replacement, "a supplied file must replace the project's file at the same path");
                await AssertServedAsync("twice.bin", later, "the later of two sources that give one path must win");
                await AssertServedAsync("kept.txt", Convert.FromHexString(projectContent["kept.txt"]), "the project's other content must still be served");
                using var absent = await app.Client.GetAsync("/content/absent.bin", startup.Token);
                Assert.Equal(HttpStatusCode.NotFound, absent.StatusCode);
            }
            Assert.Equal(projectContent, ContentOf(Path.Combine(project, "content")));
        }
        finally
        {
            supplied.Delete(recursive: true);
        }
    }

    /// <summary>Each file directly in <paramref name="directory"/> by name, with its bytes in hexadecimal.</summary>
    private static SortedDictionary<string, string> ContentOf(string directory) =>
        new(Directory.GetFiles(directory).ToDictionary(file => Path.GetFileName(file), file => Convert.ToHexString(File.ReadAllBytes(file))));

    private static byte[] RandomBytes() => System.Security.Cryptography.RandomNumberGenerator.GetBytes(257);

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
