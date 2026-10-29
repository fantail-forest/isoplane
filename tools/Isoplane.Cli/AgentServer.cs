using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;

namespace Isoplane.Cli;

/// <summary>
/// The Model Context Protocol server a coding agent starts as <c>isoplane agent mcp</c>: tools over
/// standard input and output, which is why nothing here writes to standard output, and logging on
/// standard error. Serves until the agent closes the connection, stopping every session it started.
/// Tool calls that do what a command does are recorded as that command, on the server run's telemetry.
/// </summary>
internal static class AgentServer
{
    internal static async Task<int> RunAsync(CommandTelemetry telemetry, CancellationToken cancellation)
    {
        var builder = Host.CreateApplicationBuilder(new HostApplicationBuilderSettings { DisableDefaults = true });
        builder.Logging.AddConsole(console => console.LogToStandardErrorThreshold = LogLevel.Trace);
        builder.Logging.SetMinimumLevel(LogLevel.Warning);
        builder.Services.AddSingleton(telemetry);
        // The registry stops every application still running when the host shuts down.
        builder.Services.AddSingleton<AgentSessions>();
        builder.Services.AddHostedService(services => services.GetRequiredService<AgentSessions>());
        builder.Services
            .AddMcpServer(options =>
            {
                options.ServerInfo = new() { Name = "isoplane", Version = ReleaseIdentity.OfRunningCli().Version };
                options.ServerInstructions = "Runs Isoplane applications on this machine. Start one with start_session, read what it reports with session_events, and stop it with stop_session. check_machine says whether the machine is set up.";
            })
            .WithStdioServerTransport()
            .WithTools<AgentTools>();
        using var host = builder.Build();
        await host.RunAsync(cancellation);
        return 0;
    }
}
