using Isoplane.Testing;
using Xunit;

public sealed class ApplicationTests
{
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
}
