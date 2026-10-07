using Isoplane.Testing;

namespace Sample_Handler.Tests;

/// <summary>
/// Runs the application in the Isoplane sandbox, once for every test in the
/// collection. Starting it builds the project and prepares its template, which
/// takes seconds; sharing one session keeps the tests fast.
/// </summary>
public sealed class SandboxFixture : IAsyncLifetime
{
    private IsoplaneApplication? application;

    /// <summary>Sends requests to the running application.</summary>
    public HttpClient Client => application!.Client;

    /// <summary>The development tool's recent output, for the message of a failed assertion.</summary>
    public string Diagnostics => application!.DiagnosticOutput;

    public async Task InitializeAsync()
    {
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
        application = await IsoplaneApplication.StartAsync(
            // The application's project directory, found from this test assembly's output directory.
            Path.GetFullPath("../../../../../src/Sample-Handler", AppContext.BaseDirectory),
            new ApplicationOptions
            {
                // The session reads the same settings as `dotnet run`: the project's
                // User Secrets (dotnet user-secrets set "Name" "value" --project src/Sample-Handler)
                // and the environment variables of the launch profile in
                // src/Sample-Handler/Properties/launchSettings.json. Settings given
                // here override both, for this session only, so a test does not
                // depend on what a developer has stored on their machine.
                Settings = new Dictionary<string, string> { ["Greeting:Default"] = "tester" }

                // Other options:
                //   Configuration = "Release"       builds the application's Release configuration
                //   CliPath = "/path/to/isoplane"   uses a tool that is not on PATH
            },
            startup.Token);
    }

    public async Task DisposeAsync()
    {
        if (application is not null) await application.DisposeAsync();
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
