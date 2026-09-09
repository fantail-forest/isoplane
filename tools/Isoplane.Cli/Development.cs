using System.Threading.Channels;
using System.Text.Json;

namespace Wisp.Cli;

internal static class Development
{
    internal static async Task<int> RunAsync(DevelopmentOptions options, string runtime, CancellationToken cancellation)
    {
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        using var changes = new SourceChanges(options.Directory);
        var result = await BuildAsync(options.Directory, runtime, changes, lifetime.Token);
        if (result != 0)
            return result;
        List<string> hostArguments = ["dev", "--watch", options.Directory, "--no-build", "--watch-module-only"];
        if (options.Port is { } port)
            hostArguments.AddRange(["--port", port.ToString()]);
        if (!options.Watch)
            hostArguments.Add("--no-watch");
        using var host = Processes.Start(runtime, hostArguments, options.Directory, runtime);
        var exited = host.WaitForExitAsync(lifetime.Token);
        try
        {
            if (!options.Watch)
            {
                await exited;
                return host.ExitCode;
            }
            while (true)
            {
                var changed = changes.WaitAsync(lifetime.Token);
                if (await Task.WhenAny(exited, changed) == exited)
                {
                    await exited;
                    return host.ExitCode;
                }
                await changed;
                using var buildLifetime = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
                var build = BuildAsync(options.Directory, runtime, changes, buildLifetime.Token);
                if (await Task.WhenAny(build, exited) == exited)
                {
                    buildLifetime.Cancel();
                    try { await build; }
                    catch (OperationCanceledException) when (buildLifetime.IsCancellationRequested)
                    {
                        Console.Error.WriteLine("isoplane: cancelled publication because the runtime exited.");
                    }
                    await exited;
                    return host.ExitCode;
                }
                if (await build != 0)
                    Console.Error.WriteLine("isoplane: rebuild failed; keeping the last working application. Edit the source to retry.");
            }
        }
        finally
        {
            lifetime.Cancel();
            await Processes.StopAsync(host);
        }
    }

    private static async Task<int> BuildAsync(string directory, string runtime, SourceChanges changes, CancellationToken cancellation)
    {
        var staging = Path.Combine(directory, ".wisp", "publish", Guid.NewGuid().ToString("N"));
        try
        {
            var module = await ModuleAsync(directory, runtime, cancellation);
            changes.SetOutput(module);
            Directory.CreateDirectory(staging);
            var project = Directory.GetFiles(directory, "*.csproj").Single();
            var result = await Processes.RunAsync("dotnet", ["publish", project, "-c", "Release", "-o", staging, "--disable-build-servers"], directory, runtime, cancellation);
            if (result != 0)
                return result;
            var modules = Directory.GetFiles(staging, "*.wasm");
            if (modules.Length != 1)
                throw new InvalidOperationException("Publication must produce exactly one WASM module.");
            Directory.CreateDirectory(Path.GetDirectoryName(module)!);
            File.Move(modules[0], module, overwrite: true);
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

    private static async Task<string> ModuleAsync(string directory, string runtime, CancellationToken cancellation)
    {
        using var process = Processes.Start(runtime, ["project", "--directory", directory], directory, runtime, captureOutput: true);
        try
        {
            var output = await process.StandardOutput.ReadToEndAsync(cancellation);
            await process.WaitForExitAsync(cancellation);
            if (process.ExitCode != 0)
                throw new InvalidOperationException("The runtime could not resolve this project's wisp.toml.");
            using var metadata = JsonDocument.Parse(output);
            return metadata.RootElement.GetProperty("module").GetString()
                ?? throw new InvalidOperationException("The runtime did not provide the project's module path.");
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
