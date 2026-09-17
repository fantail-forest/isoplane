using System.Threading.Channels;
using Wisp.Native;

namespace Wisp.Cli;

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
        if (options.GuestRuntime is not null)
            await RuntimeProfileAsync(options, cancellation);
        if (options.Debug)
            return await DebugDevelopment.RunAsync(options, cancellation);
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        using var changes = new SourceChanges(options.Directory);
        var result = 0;
        if (options.NoBuild)
        {
            var module = new LocalProject(options.Directory).Module;
            changes.SetOutput(module);
            if (options.Module is { } published)
                PublishModule(published, module);
            if (!File.Exists(module))
                throw new InvalidOperationException($"No built module at {module}; run dotnet build first.");
        }
        else
            result = await BuildAsync(options, changes, lifetime.Token);
        if (result != 0)
            return result;
        var configuration = await LocalSecrets.ReadAsync(options, cancellation);
        var project = new LocalProject(options.Directory);
        await using var host = new LocalSession(await File.ReadAllBytesAsync(project.Module, lifetime.Token),
            project.SessionOptions(options.Port, configuration), message => LocalEvents.Write(message, options.Json));
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
                var build = BuildAsync(options, changes, buildLifetime.Token);
                if (await Task.WhenAny(build, exited) == exited)
                {
                    buildLifetime.Cancel();
                    try { await build; }
                    catch (OperationCanceledException) when (buildLifetime.IsCancellationRequested)
                    {
                        Console.Error.WriteLine("isoplane: cancelled publication because the runtime exited.");
                    }
                    await exited;
                    return 0;
                }
                if (await build != 0)
                    Console.Error.WriteLine("isoplane: rebuild failed; keeping the last working application. Edit the source to retry.");
                else
                {
                    project = new LocalProject(options.Directory);
                    host.Reload(await File.ReadAllBytesAsync(project.Module, lifetime.Token), project.SessionOptions(options.Port, configuration));
                }
            }
        }
        finally
        {
            lifetime.Cancel();
            host.Stop();
        }
    }

    private static async Task<int> BuildAsync(DevelopmentOptions options, SourceChanges changes, CancellationToken cancellation)
    {
        var directory = options.Directory;
        var staging = Path.Combine(directory, ".wisp", "publish", Guid.NewGuid().ToString("N"));
        try
        {
            var module = new LocalProject(directory).Module;
            changes.SetOutput(module);
            Directory.CreateDirectory(staging);
            var project = Directory.GetFiles(directory, "*.csproj").Single();
            List<string> arguments = ["publish", project, "-c", options.Configuration, "-o", staging, "--disable-build-servers"];
            if (options.GuestRuntime is not null)
                arguments.Add("-p:WispGuestRuntime=" + (options.GuestRuntime.Value switch
                {
                    GuestRuntimeProfile.Mono => "Mono",
                    GuestRuntimeProfile.NativeAot => "NativeAOT",
                    _ => throw new ArgumentOutOfRangeException(nameof(options.GuestRuntime))
                }));
            var result = await Processes.RunAsync("dotnet", arguments, directory, cancellation, redirectOutput: options.Json);
            if (result != 0)
                return result;
            var modules = Directory.GetFiles(staging, "*.wasm");
            if (modules.Length != 1)
                throw new InvalidOperationException($"Publication must produce exactly one WASM module; found {modules.Length} in {staging}: {string.Join(", ", modules.Select(Path.GetFileName))}.");
            PublishModule(modules[0], module);
            return 0;
        }
        catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            Console.Error.WriteLine($"isoplane: publication failed: {error.Message}");
            return 1;
        }
        finally
        {
            try
            {
                if (Directory.Exists(staging))
                    Directory.Delete(staging, recursive: true);
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                Console.Error.WriteLine($"isoplane: could not remove publication directory {staging}: {error.Message}");
            }
        }
    }

    private static void PublishModule(string source, string destination)
    {
        if (!File.Exists(source))
            throw new InvalidOperationException($"No built module at {source}; run dotnet build first.");
        if (Path.GetFullPath(source) == Path.GetFullPath(destination))
            return;
        Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
        var staging = destination + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.Copy(source, staging);
            File.Move(staging, destination, overwrite: true);
            if (File.Exists(source + ".runtime"))
                File.Copy(source + ".runtime", destination + ".runtime", overwrite: true);
        }
        finally
        {
            File.Delete(staging);
        }
    }

    internal static async Task<GuestRuntimeProfile> RuntimeProfileAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var project = Directory.GetFiles(options.Directory, "*.csproj").Single();
        using var process = Processes.Start("dotnet", ["msbuild", project, "-p:Configuration=" + options.Configuration, "-getProperty:WispGuestRuntime"], options.Directory, captureOutput: true);
        try
        {
            var output = await process.StandardOutput.ReadToEndAsync(cancellation);
            await process.WaitForExitAsync(cancellation);
            if (process.ExitCode != 0)
                throw new InvalidOperationException("Could not evaluate the project's selected runtime; fix the MSBuild diagnostics above.");
            var selected = output.Trim() switch
            {
                "Mono" => GuestRuntimeProfile.Mono,
                "NativeAOT" => GuestRuntimeProfile.NativeAot,
                _ => throw new InvalidOperationException("The project did not select a supported WISP guest runtime.")
            };
            if (options.GuestRuntime is { } requested && requested != selected)
                throw new ArgumentException("--guest-runtime conflicts with the project configuration. Select the intended configuration with --configuration instead.");
            return selected;
        }
        finally
        {
            await Processes.StopAsync(process);
        }
    }


}

internal sealed class SourceChanges : IDisposable
{
    private readonly FileSystemWatcher watcher;
    private string? output;

    internal void SetOutput(string module) => Volatile.Write(ref output, Path.GetFullPath(module));
    private readonly Channel<bool> changes = Channel.CreateBounded<bool>(new BoundedChannelOptions(1)
    {
        FullMode = BoundedChannelFullMode.DropOldest,
        SingleReader = true
    });

    internal SourceChanges(string directory)
    {
        watcher = new FileSystemWatcher(directory) { IncludeSubdirectories = true };
        watcher.Changed += (_, e) => Observe(e.FullPath);
        watcher.Created += (_, e) => Observe(e.FullPath);
        watcher.Deleted += (_, e) => Observe(e.FullPath);
        watcher.Renamed += (_, e) => { Observe(e.OldFullPath); Observe(e.FullPath); };
        watcher.Error += (_, e) => changes.Writer.TryComplete(e.GetException());
        watcher.EnableRaisingEvents = true;
    }

    private void Observe(string path)
    {
        if (Path.GetFullPath(path) == Volatile.Read(ref output) || Directory.Exists(path))
            return;
        var relative = Path.GetRelativePath(watcher.Path, path);
        if (relative.Split(Path.DirectorySeparatorChar).Any(part => part.StartsWith('.') || part is "bin" or "obj" or "out" or "node_modules" or "target"))
            return;
        // Coalesce source events while a build is already running.
        changes.Writer.TryWrite(true);
    }

    internal async Task WaitAsync(CancellationToken cancellation) => await changes.Reader.ReadAsync(cancellation);

    public void Dispose() => watcher.Dispose();
}
