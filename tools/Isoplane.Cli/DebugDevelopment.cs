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
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        using var changes = new SourceChanges(options.Directory);
        var package = Path.Combine(session, "application.tar");
        SharedRuntime runtime;
        try { runtime = await ManagedBundle.CreateAsync(new(options.Directory, options.Configuration, package, options.NoBuild), cancellation); }
        catch (InvalidOperationException error)
        {
            Console.Error.WriteLine($"isoplane: package build failed: {error.Message}");
            return 1;
        }
        var module = runtime.DebugModule;
        using var publications = new SemaphoreSlim(1, 1);
        bool debugging = true;
        if (options.Json)
            Console.WriteLine(JsonSerializer.Serialize(new { @event = "debugger_ready", endpoint = relay.AdapterEndpoint.ToString(), module, package, runtime = "mono" }));
        else
            Console.WriteLine($"Debugger target: {relay.AdapterEndpoint}. Connect your editor through isoplane debug-adapter; the connection remains available across HTTP requests.");
        await using var host = new LocalSession(await File.ReadAllBytesAsync(module, cancellation),
            new LocalProject(options.Directory).SessionOptions(options.Port, [], endpoint, Path.Combine(session, "runtime"), package: package),
            message => LocalEvents.Write(message, options.Json));
        relay.PrepareDetach = async token =>
        {
            await publications.WaitAsync(token);
            try
            {
                await host.PrepareDetachAsync(await File.ReadAllBytesAsync(runtime.Module, token),
                    new LocalProject(options.Directory).SessionOptions(options.Port, [], null, Path.Combine(session, "runtime"), package: package));
                debugging = false;
            }
            finally { publications.Release(); }
        };
        var running = RunHostAsync(host, relay, lifetime.Token);
        try
        {
            if (options.Watch)
                await SourceReload.RunAsync(changes, running, async token =>
                {
                    await publications.WaitAsync(token);
                    try
                    {
                        var replacement = await ManagedBundle.CreateAsync(new(options.Directory, options.Configuration, package, options.NoBuild), token);
                        host.Reload(await File.ReadAllBytesAsync(debugging ? replacement.DebugModule : replacement.Module, token),
                            new LocalProject(options.Directory).SessionOptions(options.Port, [], debugging ? endpoint : null, Path.Combine(session, "runtime"), package: package));
                        runtime = replacement;
                        LocalEvents.Write(new System.Text.Json.Nodes.JsonObject { ["event"] = "reload_pending" }, options.Json);
                    }
                    catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
                    {
                        LocalEvents.Write(new System.Text.Json.Nodes.JsonObject { ["event"] = "reload_failed", ["detail"] = error.Message }, options.Json);
                    }
                    finally { publications.Release(); }
                }, lifetime.Token);
            else
                await running;
            return 0;
        }
        finally
        {
            lifetime.Cancel();
            host.Stop();
            await running;
        }
    }

    internal static async Task RunHostAsync(LocalSession host, DebugRelay relay, CancellationToken cancellation)
    {
        using var stop = cancellation.Register(host.Stop);
        try
        {
            if (await Task.WhenAny(host.Completion, relay.Completion) == relay.Completion)
            {
                await relay.Completion;
                if (!relay.Detached) host.Stop();
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
