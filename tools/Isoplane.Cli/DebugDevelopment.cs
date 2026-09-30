using System.Text.Json;
using Isoplane.Native;

namespace Isoplane.Cli;

internal static class DebugDevelopment
{
    internal static async Task<int> RunAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var session = Path.Combine(options.Directory, ".isoplane", "debug", Guid.NewGuid().ToString("N"));
        try
        {
            if (OperatingSystem.IsWindows())
                Directory.CreateDirectory(session);
            else
                Directory.CreateDirectory(session, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
            await using var relay = new DebugRelay(cancellation);
            return await RunPackageAsync(options, session, relay.Endpoint.ToString(), relay, cancellation);
        }
        finally
        {
            try
            {
                if (Directory.Exists(session))
                    Directory.Delete(session, recursive: true);
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                Console.Error.WriteLine($"isoplane: could not remove debug session {session}: {error.Message}");
            }
        }
    }

    /// <summary>
    /// Debugs an interpreted project's package on the SDK's local-debug runtime,
    /// which connects its agent to <paramref name="endpoint"/> through the relay.
    /// </summary>
    private static async Task<int> RunPackageAsync(DevelopmentOptions options, string session, string endpoint, DebugRelay relay, CancellationToken cancellation)
    {
        var package = Path.Combine(session, "application.tar");
        SharedRuntime runtime;
        try { runtime = await ManagedBundle.CreateAsync(new(options.Directory, options.Configuration, package, options.NoBuild), cancellation); }
        catch (InvalidOperationException error)
        {
            Console.Error.WriteLine($"isoplane: package build failed: {error.Message}");
            return 1;
        }
        var module = runtime.DebugModule;
        if (options.Json)
            Console.WriteLine(JsonSerializer.Serialize(new { @event = "debugger_ready", endpoint = relay.AdapterEndpoint.ToString(), module, package, runtime = "mono" }));
        else
            Console.WriteLine($"Debugger target: {relay.AdapterEndpoint}. Connect your editor through isoplane debug-adapter; the connection remains available across HTTP requests.");
        await using var host = new LocalSession(await File.ReadAllBytesAsync(module, cancellation),
            new LocalProject(options.Directory).SessionOptions(options.Port, [], endpoint, Path.Combine(session, "runtime"), package: package),
            message => LocalEvents.Write(message, options.Json));
        await RunHostAsync(host, relay, cancellation);
        return 0;
    }

    internal static async Task RunHostAsync(LocalSession host, DebugRelay relay, CancellationToken cancellation)
    {
        using var stop = cancellation.Register(host.Stop);
        try
        {
            if (await Task.WhenAny(host.Completion, relay.Completion) == relay.Completion)
            {
                await relay.Completion;
                host.Stop();
            }
            await host.Completion;
        }
        finally
        {
            host.Stop();
            await host.Completion;
        }
    }
}
