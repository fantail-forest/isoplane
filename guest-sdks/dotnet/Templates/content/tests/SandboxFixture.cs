using Isoplane.Testing;

namespace Sample_App.Tests;

/// <summary>
/// Runs the application in the Isoplane sandbox, once for every test in the
/// collection. Starting it builds the project and prepares its template, which
/// takes seconds; sharing one session keeps the tests fast.
/// </summary>
/// <remarks>
/// The pattern for anything the application depends on, such as a database or
/// another service, is to start a real or stand-in one on this machine in
/// <see cref="InitializeAsync"/>, then point the application at it through
/// <see cref="ApplicationOptions.Settings"/>.
/// </remarks>
public sealed class SandboxFixture : IAsyncLifetime
{
    private IsoplaneApplication? application;

    /// <summary>Sends requests to the running application.</summary>
    public HttpClient Client => application!.Client;

    /// <summary>The development tool's recent output, for the message of a failed assertion.</summary>
    public string Diagnostics => application!.DiagnosticOutput;

    public async Task InitializeAsync()
    {
        // Bounds a start that hangs. The first start on a machine also compiles the
        // shared runtime, so allow more than an ordinary start needs.
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
        application = await IsoplaneApplication.StartAsync(
            // The application's project directory, found from this test assembly's
            // output directory (bin/<Configuration>/net10.0). Change it here and in
            // the project file's ProjectReference if the application moves.
            Path.GetFullPath("../../../APPLICATION_DIRECTORY", AppContext.BaseDirectory),
            new ApplicationOptions
            {
                // The session reads the same settings as `dotnet run`: the
                // application's appsettings files, its User Secrets and the
                // environment variables of its launch profile. Settings given here
                // override all of them, for this session only, so a test does not
                // depend on what a developer has stored on their machine:
                //
                //   Settings = new Dictionary<string, string> { ["ConnectionStrings:Main"] = connectionString },
                //
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
