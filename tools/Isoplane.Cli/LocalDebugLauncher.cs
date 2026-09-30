using System.Globalization;
using System.Net;
using System.Text.Json.Nodes;
using Isoplane.Debugging;
using Isoplane.Processes;

namespace Isoplane.Cli;

internal sealed class LocalDebugLauncher : DebugLauncher
{
    /// <summary>Starts the installed CLI's local debugger and owns its complete process tree.</summary>
    public async Task RunAsync(DebugLaunchOptions options, Action<IPEndPoint> ready, Action<string> output, CancellationToken cancellation)
    {
        string[] arguments = [typeof(LocalDebugLauncher).Assembly.Location, "dev", "--project", options.Project,
            "--configuration", options.Configuration, "--debug", "--json", "--shutdown-on-stdin-close"];
        if (!options.Watch) arguments = [.. arguments, "--no-watch"];
        if (options.Port is { } port) arguments = [.. arguments, "--port", port.ToString(CultureInfo.InvariantCulture)];
        using var process = Processes.Start("dotnet", arguments, options.Project, captureOutput: true, captureErrors: true, shutdownGrace: TimeSpan.FromSeconds(10));
        var stdout = ForwardAsync(process, process.StandardOutput, line => Report(line, ready, output));
        var stderr = ForwardAsync(process, process.StandardError, line => Report(line, ready, output));
        try
        {
            await process.WaitForExitAsync(cancellation);
            await Task.WhenAll(stdout, stderr);
            if (process.ExitCode != 0)
                throw new InvalidOperationException($"The local application exited with code {process.ExitCode}; see the debugger console for build or startup errors.");
        }
        finally
        {
            await Processes.StopAsync(process);
            await Task.WhenAll(stdout, stderr);
        }
    }

    private static async Task ForwardAsync(OwnedProcess process, StreamReader stream, Action<string> output)
    {
        try
        {
            while (await stream.ReadLineAsync() is { } line) output(line);
        }
        catch
        {
            process.StandardInput.Close();
            throw;
        }
    }

    private static void Report(string line, Action<IPEndPoint> ready, Action<string> output)
    {
        JsonObject? message;
        try { message = JsonNode.Parse(line) as JsonObject; }
        catch (System.Text.Json.JsonException) { output(line); return; }
        if (message?["event"]?.GetValue<string>() != "debugger_ready")
        {
            if (message?["event"] is not null) LocalEvents.Write(message, json: false, output);
            else output(line);
            return;
        }
        if (message["endpoint"]?.GetValue<string>() is not { } address || !IPEndPoint.TryParse(address, out var endpoint)
            || !endpoint.Address.Equals(IPAddress.Loopback) || endpoint.Port == 0)
            throw new IOException("The local application returned an invalid debugger endpoint.");
        ready(endpoint);
    }
}
