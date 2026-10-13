using System.Net.Sockets;
using System.Text.Json;
using System.Text.Json.Nodes;
using Isoplane.Native;
using Isoplane.Build;

namespace Isoplane.Cli;

internal static class DebugDevelopment
{
    internal static async Task<int> RunAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        var session = new LocalProject(options.Directory).Scratch("debug");
        try
        {
            if (OperatingSystem.IsWindows())
                Directory.CreateDirectory(session);
            else
                Directory.CreateDirectory(session, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
            var package = Path.Combine(session, "application.tar");
            await using var relay = new DebugRelay(package, lifetime.Token,
                new DebugProject(ManagedBundle.ProjectFile(options.Directory), new() { ["Configuration"] = options.Configuration }));
            if (options.DebugLaunchEndpoint is not { } launchEndpoint)
                return await RunPackageAsync(options, session, package, relay.Endpoint.ToString(), relay, null, lifetime.Token);
            using var client = new TcpClient();
            await client.ConnectAsync(launchEndpoint, lifetime.Token);
            await using var launch = new TerminalDebugLaunch(client, relay.EditorConnected, lifetime);
            try { return await RunPackageAsync(options, session, package, relay.Endpoint.ToString(), relay, launch, lifetime.Token); }
            finally { lifetime.Cancel(); }
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
    private static async Task<int> RunPackageAsync(DevelopmentOptions options, string session, string package, string endpoint, DebugRelay relay, TerminalDebugLaunch? launch, CancellationToken cancellation)
    {
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        using var changes = new SourceChanges(options.Directory);
        SharedRuntime runtime;
        try { runtime = await ManagedBundle.CreateAsync(new(options.Directory, options.Configuration, package, options.NoBuild), cancellation); }
        catch (InvalidOperationException error)
        {
            Console.Error.WriteLine($"isoplane: package build failed: {error.Message}");
            return 1;
        }
        var module = runtime.DebugModule;
        var settings = await DevelopmentSettings.ResolveAsync(options, cancellation);
        using var publications = new SemaphoreSlim(1, 1);
        bool debugging = true;
        if (options.Json)
            Console.WriteLine(JsonSerializer.Serialize(new { @event = "debugger_ready", endpoint = relay.AdapterEndpoint.ToString(), module, package, runtime = "mono" }));
        else
            Console.WriteLine($"Debugger target: {relay.AdapterEndpoint}. Connect your editor through isoplane debug-adapter; the connection remains available across HTTP requests.");
        var ready = new TaskCompletionSource<JsonObject>(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var host = new LocalSession(await File.ReadAllBytesAsync(module, cancellation),
            new LocalProject(options.Directory).SessionOptions(settings, endpoint, Path.Combine(session, "runtime"), package: package),
            message =>
            {
                LocalEvents.Write(message, options.Json);
                if (message["event"]?.GetValue<string>() == "ready") ready.TrySetResult(message);
            });
        relay.PrepareDetach = async token =>
        {
            await publications.WaitAsync(token);
            try
            {
                await host.PrepareDetachAsync(await File.ReadAllBytesAsync(runtime.Module, token),
                    new LocalProject(options.Directory).SessionOptions(settings, null, Path.Combine(session, "runtime"), package: package));
                debugging = false;
            }
            finally { publications.Release(); }
        };
        var running = RunHostAsync(host, relay, lifetime.Token);
        try
        {
            if (launch is not null)
            {
                if (await Task.WhenAny(ready.Task, running) == running)
                {
                    await running;
                    throw new IOException("The local application exited before its HTTP listener was ready.");
                }
                await launch.AnnounceAsync(relay.AdapterEndpoint.ToString(), await ready.Task, lifetime.Token);
            }
            if (options.Watch)
                await SourceReload.RunAsync(changes, running, async token =>
                {
                    await publications.WaitAsync(token);
                    try
                    {
                        var replacement = await ManagedBundle.CreateAsync(new(options.Directory, options.Configuration, package, options.NoBuild), token);
                        host.Reload(await File.ReadAllBytesAsync(debugging ? replacement.DebugModule : replacement.Module, token),
                            new LocalProject(options.Directory).SessionOptions(settings, debugging ? endpoint : null, Path.Combine(session, "runtime"), package: package));
                        runtime = replacement;
                        if (debugging) await relay.PackageChangedAsync(token);
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
