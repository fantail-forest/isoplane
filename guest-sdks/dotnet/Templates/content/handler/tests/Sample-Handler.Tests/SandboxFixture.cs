using Isoplane.Testing;

namespace Sample_Handler.Tests;

/// <summary>Runs the application in the Isoplane sandbox, once for every test in the collection.</summary>
public sealed class SandboxFixture : IAsyncLifetime
{
    private IsoplaneApplication? application;

    public HttpClient Client => application!.Client;

    /// <summary>The development tool's recent output, for the message of a failed assertion.</summary>
    public string Diagnostics => application!.DiagnosticOutput;

    public async Task InitializeAsync()
    {
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
        application = await IsoplaneApplication.StartAsync(
            Path.GetFullPath("../../../../../src/Sample-Handler", AppContext.BaseDirectory),
            new ApplicationOptions
            {
                // Settings override the launch profile and User Secrets for this session only.
                Settings = new Dictionary<string, string> { ["Greeting:Default"] = "tester" }
            },
            startup.Token);
    }

    public async Task DisposeAsync()
    {
        if (application is not null) await application.DisposeAsync();
    }
}

[CollectionDefinition(Name)]
public sealed class SandboxCollection : ICollectionFixture<SandboxFixture>
{
    public const string Name = "Sandbox";
}
