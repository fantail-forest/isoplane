using Isoplane.Testing;
using ModelContextProtocol.Client;

namespace Sample_McpServer.Tests;

/// <summary>
/// Runs the application in the Isoplane sandbox and connects an MCP client to
/// it. Every test in the collection shares it: starting the session builds the
/// project and prepares its template, which takes seconds.
/// </summary>
/// <remarks>
/// The pattern for anything the application depends on, such as a service a tool
/// calls, is to start a real or stand-in one on this machine in
/// <see cref="InitializeAsync"/>, then point the application at it through
/// <see cref="ApplicationOptions.Settings"/>.
/// </remarks>
public sealed class SandboxFixture : IAsyncLifetime
{
    /// <summary>The value the session gives "Handbook:MaxResults", below what the application ships with.</summary>
    public const int MaxResults = 2;

    private IsoplaneApplication? application;
    private McpClient? mcp;

    /// <summary>Sends plain HTTP requests to the running application.</summary>
    public HttpClient Client => application!.Client;

    /// <summary>Calls the application as an MCP client would: lists its tools and invokes them.</summary>
    public McpClient Mcp => mcp!;

    /// <summary>The development tool's recent output, for the message of a failed assertion.</summary>
    public string Diagnostics => application!.DiagnosticOutput;

    public async Task InitializeAsync()
    {
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(10));
        application = await IsoplaneApplication.StartAsync(
            // The application's project directory, found from this test assembly's output directory.
            Path.GetFullPath("../../../../../src/Sample-McpServer", AppContext.BaseDirectory),
            new ApplicationOptions
            {
                // The session reads the same configuration as `dotnet run`: the
                // appsettings files, the project's User Secrets
                // (dotnet user-secrets set "Name" "value" --project src/Sample-McpServer)
                // and the environment variables of the launch profile in
                // src/Sample-McpServer/Properties/launchSettings.json. Settings given
                // here override all of them, for this session only, so a test does
                // not depend on what a developer has stored on their machine.
                Settings = new Dictionary<string, string>
                {
                    ["Handbook:MaxResults"] = MaxResults.ToString()
                }

                // Other options:
                //   Configuration = "Release"       builds the application's Release configuration
                //   CliPath = "/path/to/isoplane"   uses a tool that is not on PATH
            },
            startup.Token);

        // The same transport a real client uses, over the session's HttpClient.
        // The fixture keeps ownership of that client, hence ownsHttpClient: false.
        var transport = new HttpClientTransport(
            new HttpClientTransportOptions
            {
                Endpoint = new Uri(Client.BaseAddress!, "/mcp"),
                TransportMode = HttpTransportMode.StreamableHttp
            },
            Client,
            ownsHttpClient: false);
        mcp = await McpClient.CreateAsync(transport, cancellationToken: startup.Token);
    }

    public async Task DisposeAsync()
    {
        if (mcp is not null) await mcp.DisposeAsync();
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
