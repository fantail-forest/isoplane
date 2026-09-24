using Isoplane.Native;

namespace Isoplane.Cli;

internal static class Development
{
    internal static async Task<int> PrepareAsync(string[] arguments, CancellationToken cancellation)
    {
        var options = DevelopmentOptions.Parse(arguments, preparing: true);
        var configuration = await LocalSecrets.ReadAsync(options, cancellation);
        var project = new LocalProject(options.Directory);
        var module = await File.ReadAllBytesAsync(options.Module ?? project.Module, cancellation);
        var report = LocalPreparation.Prepare(module, project.SessionOptions(null, configuration));
        if (options.Json) Console.WriteLine(report.ToJsonString());
        return 0;
    }

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
        var profile = await RuntimeProfileAsync(options, cancellation);
        if (options.Debug)
            return await DebugDevelopment.RunAsync(options, cancellation);
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        using var changes = new SourceChanges(options.Directory);
        LocalBuild build = profile == GuestRuntimeProfile.Mono ? new PackageBuild(options) : new ModuleBuild(options, changes);
        var result = 0;
        if (options.NoBuild)
            build.UsePublished();
        else
            result = await build.BuildAsync(lifetime.Token);
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

    /// <summary>What a session runs: the module, and for an interpreted project, the package it loads.</summary>
    private abstract class LocalBuild(DevelopmentOptions options)
    {
        protected DevelopmentOptions Options { get; } = options;

        /// <summary>Reuses what an earlier build published, for <c>--no-build</c>.</summary>
        internal abstract void UsePublished();

        internal abstract Task<int> BuildAsync(CancellationToken cancellation);

        internal abstract Task<byte[]> ModuleAsync(CancellationToken cancellation);

        internal abstract byte[] SessionOptions(Dictionary<string, string> configuration);
    }

    /// <summary>A NativeAOT project, published as a module of its own.</summary>
    private sealed class ModuleBuild(DevelopmentOptions options, SourceChanges changes) : LocalBuild(options)
    {
        internal override void UsePublished()
        {
            var module = new LocalProject(Options.Directory).Module;
            changes.SetOutput(module);
            if (Options.Module is { } published)
                PublishModule(published, module);
            if (!File.Exists(module))
                throw new InvalidOperationException($"No built module at {module}; run dotnet build first.");
        }

        internal override Task<int> BuildAsync(CancellationToken cancellation) => Development.BuildAsync(Options, changes, cancellation);

        internal override Task<byte[]> ModuleAsync(CancellationToken cancellation) =>
            File.ReadAllBytesAsync(new LocalProject(Options.Directory).Module, cancellation);

        internal override byte[] SessionOptions(Dictionary<string, string> configuration) =>
            new LocalProject(Options.Directory).SessionOptions(Options.Port, configuration);
    }

    /// <summary>An interpreted project, built as a package for the shared runtime its SDK carries.</summary>
    private sealed class PackageBuild(DevelopmentOptions options) : LocalBuild(options)
    {
        private string? runtime;

        internal override void UsePublished() =>
            throw new ArgumentException("An interpreted project runs the package isoplane dev builds; omit --no-build and --module.");

        internal override async Task<int> BuildAsync(CancellationToken cancellation)
        {
            var package = new LocalProject(Options.Directory).Package;
            try
            {
                Directory.CreateDirectory(Path.GetDirectoryName(package)!);
                runtime = (await ManagedBundle.CreateAsync(new(Options.Directory, Options.Configuration, package), cancellation)).Module;
                return 0;
            }
            catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
            {
                Console.Error.WriteLine($"isoplane: package build failed: {error.Message}");
                return 1;
            }
        }

        internal override Task<byte[]> ModuleAsync(CancellationToken cancellation) =>
            File.ReadAllBytesAsync(runtime ?? throw new InvalidOperationException("The package has not been built."), cancellation);

        internal override byte[] SessionOptions(Dictionary<string, string> configuration)
        {
            var project = new LocalProject(Options.Directory);
            return project.SessionOptions(Options.Port, configuration, package: project.Package);
        }
    }

    private static async Task<int> BuildAsync(DevelopmentOptions options, SourceChanges changes, CancellationToken cancellation)
    {
        var directory = options.Directory;
        var staging = Path.Combine(directory, ".isoplane", "publish", Guid.NewGuid().ToString("N"));
        try
        {
            var module = new LocalProject(directory).Module;
            changes.SetOutput(module);
            Directory.CreateDirectory(staging);
            var project = Directory.GetFiles(directory, "*.csproj").Single();
            List<string> arguments = ["publish", project, "-c", options.Configuration, "-o", staging, "--disable-build-servers"];
            if (options.GuestRuntime is not null)
                arguments.Add("-p:IsoplaneGuestRuntime=" + (options.GuestRuntime.Value switch
                {
                    GuestRuntimeProfile.Mono => "Mono",
                    GuestRuntimeProfile.NativeAot => "NativeAOT",
                    _ => throw new ArgumentOutOfRangeException(nameof(options.GuestRuntime))
                }));
            var result = await Processes.RunAsync("dotnet", arguments, directory, cancellation, redirectOutput: options.Json);
            if (result != 0)
            {
                Console.Error.WriteLine($"isoplane: publication failed with exit code {result}.");
                return result;
            }
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
        var staging = Path.Combine(Path.GetDirectoryName(destination)!, "." + Path.GetFileName(destination) + "." + Guid.NewGuid().ToString("N") + ".tmp");
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
        using var process = Processes.Start("dotnet", ["msbuild", project, "-p:Configuration=" + options.Configuration, "-getProperty:IsoplaneGuestRuntime"], options.Directory, captureOutput: true);
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
                _ => throw new InvalidOperationException("The project did not select a supported Isoplane guest runtime.")
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
