using System.Globalization;
using System.Net;
using System.Net.Sockets;
using System.Text.Json;
using System.Text.Json.Nodes;
using Isoplane.Debugging;
using Isoplane.Native;
using Isoplane.Processes;

namespace Isoplane.Cli;

internal sealed class LocalDebugLauncher : DebugLauncher
{
    /// <summary>Starts the installed CLI under either adapter or editor-terminal ownership.</summary>
    public async Task RunAsync(DebugLaunchOptions options, Func<IPEndPoint, Task> ready, Action<string> output, Func<DebugTerminal, CancellationToken, Task>? terminal, CancellationToken cancellation)
    {
        if (terminal is not null)
        {
            await RunTerminalAsync(options, ready, output, terminal, cancellation);
            return;
        }
        var arguments = Arguments(options, json: true);
        using var process = Processes.Start("dotnet", arguments, options.Project, captureOutput: true, captureErrors: true, shutdownGrace: TimeSpan.FromSeconds(10));
        await SuperviseAsync(process, ready, output, cancellation);
    }

    /// <summary>
    /// Runs until <paramref name="process"/> exits or <paramref name="cancellation"/> is requested, then stops it.
    /// Reports the debugger endpoint the process announces to <paramref name="ready"/> and everything else it
    /// writes to <paramref name="output"/>. An event it does not know, or one that lacks one of its members, closes
    /// the process's standard input and throws <see cref="JsonException"/>.
    /// </summary>
    internal static async Task SuperviseAsync(OwnedProcess process, Func<IPEndPoint, Task> ready, Action<string> output, CancellationToken cancellation)
    {
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

    private static string[] Arguments(DebugLaunchOptions options, bool json)
    {
        string[] arguments = [typeof(LocalDebugLauncher).Assembly.Location, "dev", "--project", options.Project,
            "--configuration", options.Configuration, "--debug", "--shutdown-on-stdin-close"];
        if (json) arguments = [.. arguments, "--json"];
        if (!options.Watch) arguments = [.. arguments, "--no-watch"];
        if (options.Port is { } port) arguments = [.. arguments, "--port", port.ToString(CultureInfo.InvariantCulture)];
        return arguments;
    }

    private static async Task RunTerminalAsync(DebugLaunchOptions options, Func<IPEndPoint, Task> ready, Action<string> output,
        Func<DebugTerminal, CancellationToken, Task> terminal, CancellationToken cancellation)
    {
        using (var listener = new TcpListener(IPAddress.Loopback, 0))
        {
            listener.Start();
            await terminal(new DebugTerminal(options.Project,
                ["dotnet", .. Arguments(options, json: false), "--debug-launch-endpoint", listener.LocalEndpoint.ToString()!]), cancellation);
            using var startup = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            startup.CancelAfter(TimeSpan.FromSeconds(30));
            TcpClient accepted;
            try { accepted = await listener.AcceptTcpClientAsync(startup.Token); }
            catch (OperationCanceledException) when (!cancellation.IsCancellationRequested)
            {
                throw new IOException("The terminal did not start the local application within 30 seconds; see the Isoplane terminal.");
            }
            using var client = accepted;
            using var reader = new StreamReader(client.GetStream());
            var line = await reader.ReadLineAsync(cancellation)
                ?? throw new IOException("The local application exited during startup; see the Isoplane terminal.");
            var announcement = DebugLaunchAnnouncement.Read(line);
            if (!IPEndPoint.TryParse(announcement.Endpoint, out var endpoint)
                || !endpoint.Address.Equals(IPAddress.Loopback) || endpoint.Port == 0
                || !Uri.TryCreate(announcement.Url, UriKind.Absolute, out var url) || url.Scheme != "http" || !url.IsLoopback)
                throw new IOException("The terminal application returned invalid debugger startup information.");
            await ready(endpoint);
            output($"  ready {announcement.App} at {url}");
        }
        var stopped = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var cancelled = cancellation.Register(() => stopped.TrySetCanceled(cancellation));
        await stopped.Task;
    }

    private static async Task ForwardAsync(OwnedProcess process, StreamReader stream, Func<string, Task> output)
    {
        try
        {
            while (await stream.ReadLineAsync() is { } line) await output(line);
        }
        catch
        {
            process.StandardInput.Close();
            throw;
        }
    }

    private static async Task Report(string line, Func<IPEndPoint, Task> ready, Action<string> output)
    {
        JsonObject? reported;
        try { reported = JsonNode.Parse(line) as JsonObject; }
        catch (JsonException) { output(line); return; }
        if (reported?["event"] is null)
        {
            output(line);
            return;
        }
        var message = SessionEvent.Parse(reported);
        if (message is SessionEvent.DebuggerReady debugger)
            await ready(IPEndPoint.Parse(debugger.Endpoint));
        else
            LocalEvents.Write(message, json: false, output);
    }
}
