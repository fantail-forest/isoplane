using AngleSharp;
using Isoplane.Testing;
using Microsoft.EntityFrameworkCore;
using Sample_WebApp.Data;
using Testcontainers.PostgreSql;

namespace Sample_WebApp.Tests;

/// <summary>
/// Runs the application in the Isoplane sandbox against a disposable PostgreSQL
/// container. Every test in the collection shares it: starting the session
/// builds the project and prepares its template, which takes seconds.
/// </summary>
/// <remarks>
/// The pattern for anything the application depends on is the one used here:
/// start a real or stand-in service on this machine, then point the application
/// at it through <see cref="ApplicationOptions.Settings"/>.
/// </remarks>
public sealed class SandboxFixture : IAsyncLifetime
{
    // The same image as compose.yaml, in a container this run creates and removes.
    private readonly PostgreSqlContainer database = new PostgreSqlBuilder("postgres:17-alpine").Build();
    private IsoplaneApplication? application;

    /// <summary>Sends plain HTTP requests to the running application. It keeps no cookies.</summary>
    public HttpClient Client => application!.Client;

    /// <summary>The development tool's recent output, for the message of a failed assertion.</summary>
    public string Diagnostics => application!.DiagnosticOutput;

    /// <summary>
    /// A visitor's browser: it loads pages, keeps their cookies and submits their
    /// forms, hidden antiforgery token included. Each call is a new visitor.
    /// </summary>
    public IBrowsingContext NewBrowser() =>
        BrowsingContext.New(Configuration.Default.WithDefaultLoader().WithDefaultCookies());

    /// <summary>The address of a page of the running application.</summary>
    public string Url(string path) => new Uri(Client.BaseAddress!, path).ToString();

    /// <summary>The application's database, for a test that asserts on what was stored.</summary>
    public AppDbContext OpenDatabase() =>
        new(new DbContextOptionsBuilder<AppDbContext>().UseNpgsql(database.GetConnectionString()).Options);

    public async Task InitializeAsync()
    {
        await database.StartAsync();
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
        application = await IsoplaneApplication.StartAsync(
            // The application's project directory, found from this test assembly's output directory.
            Path.GetFullPath("../../../../../src/Sample-WebApp", AppContext.BaseDirectory),
            new ApplicationOptions
            {
                // The session reads the same configuration as `dotnet run`: the
                // appsettings files, the project's User Secrets
                // (dotnet user-secrets set "Name" "value" --project src/Sample-WebApp) and
                // the environment variables of the launch profile in
                // src/Sample-WebApp/Properties/launchSettings.json. Settings given here
                // override all of them, for this session only, so a test does not
                // depend on what a developer has stored on their machine.
                Settings = new Dictionary<string, string>
                {
                    ["ConnectionStrings:Database"] = database.GetConnectionString()
                }

                // Other options:
                //   Configuration = "Release"       builds the application's Release configuration
                //   CliPath = "/path/to/isoplane"   uses a tool that is not on PATH
            },
            startup.Token);
    }

    public async Task DisposeAsync()
    {
        if (application is not null) await application.DisposeAsync();
        await database.DisposeAsync();
    }
}

/// <summary>
/// Test classes marked <c>[Collection(SandboxCollection.Name)]</c> share the one
/// <see cref="SandboxFixture"/> and take it as a constructor parameter.
/// </summary>
[CollectionDefinition(Name)]
public sealed class SandboxCollection : ICollectionFixture<SandboxFixture>
{
    public const string Name = "Sandbox";
}
