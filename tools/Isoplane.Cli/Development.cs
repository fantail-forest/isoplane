using Isoplane.Native;

namespace Isoplane.Cli;

internal static class Development
{
    internal static async Task<int> RunAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        if (!options.ShutdownOnInputClose)
            return await RunOwnedAsync(options, cancellation);
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        var inputCancellation = lifetime.Token;
        using var standardInput = Console.OpenStandardInput();
        var input = Task.Run(() => WaitForInputCloseAsync(standardInput, inputCancellation));
        var session = RunOwnedAsync(options, lifetime.Token);
        try
        {
            if (await Task.WhenAny(session, input) == input)
            {
                await input;
                lifetime.Cancel();
            }
            return await session;
        }
        finally
        {
            // A console read can remain blocked until process exit; it owns no child work.
            lifetime.Cancel();
        }
    }

    private static async Task WaitForInputCloseAsync(Stream standardInput, CancellationToken cancellation)
    {
        var input = new byte[1];
        while (await standardInput.ReadAsync(input.AsMemory(), cancellation) != 0) { }
    }

    private static async Task<int> RunOwnedAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        if (options.Debug)
            return await DebugDevelopment.RunAsync(options, cancellation);
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        using var changes = new SourceChanges(options.Directory);
        var build = new PackageBuild(options);
        var result = await build.BuildAsync(lifetime.Token);
        if (result != 0)
            return result;
        var configuration = await LocalSecrets.ReadAsync(options, cancellation);
        await using var host = new LocalSession(await build.ModuleAsync(lifetime.Token),
            build.SessionOptions(configuration), message => LocalEvents.Write(message, options.Json));
        using var stopping = lifetime.Token.Register(host.Stop);
        var exited = host.Completion;
        try
        {
            if (!options.Watch)
            {
                await exited;
                return 0;
            }
            while (true)
            {
                var changed = changes.WaitAsync(lifetime.Token);
                if (await Task.WhenAny(exited, changed) == exited)
                {
                    await exited;
                    return 0;
                }
                await changed;
                using var buildLifetime = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
                var rebuild = build.BuildAsync(buildLifetime.Token);
                if (await Task.WhenAny(rebuild, exited) == exited)
                {
                    buildLifetime.Cancel();
                    try { await rebuild; }
                    catch (OperationCanceledException) when (buildLifetime.IsCancellationRequested)
                    {
                        Console.Error.WriteLine("isoplane: cancelled publication because the runtime exited.");
                    }
                    await exited;
                    return 0;
                }
                if (await rebuild != 0)
                    Console.Error.WriteLine("isoplane: rebuild failed; keeping the last working application. Edit the source to retry.");
                else
                    host.Reload(await build.ModuleAsync(lifetime.Token), build.SessionOptions(configuration));
            }
        }
        finally
        {
            lifetime.Cancel();
            host.Stop();
        }
    }

    /// <summary>An interpreted project, built as a package for the shared runtime its SDK carries.</summary>
    private sealed class PackageBuild(DevelopmentOptions options)
    {
        private string? runtime;

        internal async Task<int> BuildAsync(CancellationToken cancellation)
        {
            var package = new LocalProject(options.Directory).Package;
            try
            {
                Directory.CreateDirectory(Path.GetDirectoryName(package)!);
                runtime = (await ManagedBundle.CreateAsync(new(options.Directory, options.Configuration, package, options.NoBuild), cancellation)).Module;
                return 0;
            }
            catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
            {
                Console.Error.WriteLine($"isoplane: package build failed: {error.Message}");
                return 1;
            }
        }

        internal Task<byte[]> ModuleAsync(CancellationToken cancellation) =>
            File.ReadAllBytesAsync(runtime ?? throw new InvalidOperationException("The package has not been built."), cancellation);

        internal byte[] SessionOptions(Dictionary<string, string> configuration)
        {
            var project = new LocalProject(options.Directory);
            return project.SessionOptions(options.Port, configuration, package: project.Package);
        }
    }

}
