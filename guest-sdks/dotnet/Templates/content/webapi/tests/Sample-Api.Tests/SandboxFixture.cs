using Isoplane.Testing;
using Testcontainers.PostgreSql;

namespace Sample_Api.Tests;

/// <summary>
/// Runs the application in the Isoplane sandbox against a disposable PostgreSQL
/// container and a local webhook receiver. Every test in the collection shares it.
/// </summary>
public sealed class SandboxFixture : IAsyncLifetime
{
    private readonly PostgreSqlContainer database = new PostgreSqlBuilder("postgres:17-alpine").Build();
    private IsoplaneApplication? application;

    public HttpClient Client => application!.Client;

    public WebhookReceiver Webhook { get; } = new();

    /// <summary>The development tool's recent output, for the message of a failed assertion.</summary>
    public string Diagnostics => application!.DiagnosticOutput;

    public async Task InitializeAsync()
    {
        await database.StartAsync();
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
        application = await IsoplaneApplication.StartAsync(
            Path.GetFullPath("../../../../../src/Sample-Api", AppContext.BaseDirectory),
            new ApplicationOptions
            {
                // Settings override the launch profile and User Secrets for this session only.
                Settings = new Dictionary<string, string>
                {
                    ["ConnectionStrings:Todos"] = database.GetConnectionString(),
                    ["Notifications:WebhookUrl"] = Webhook.Url.ToString()
                }
            },
            startup.Token);
    }

    public async Task DisposeAsync()
    {
        if (application is not null) await application.DisposeAsync();
        await Webhook.DisposeAsync();
        await database.DisposeAsync();
    }
}

[CollectionDefinition(Name)]
public sealed class SandboxCollection : ICollectionFixture<SandboxFixture>
{
    public const string Name = "Sandbox";
}
