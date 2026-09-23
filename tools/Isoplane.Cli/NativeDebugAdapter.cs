namespace Isoplane.Cli;

internal static class NativeDebugAdapter
{
    internal const string SessionDirectoryVariable = "ISOPLANE_NATIVE_DEBUG_DIRECTORY";

    internal static async Task<int> RunAsync(CancellationToken cancellation)
    {
        var debugger = NativeDebuggerToolchain.Resolve(Toolchain.Selected(Toolchain.Home));
        await Processes.CheckAsync(debugger.Executable, ["--help"], Environment.CurrentDirectory, cancellation, debugger.Environment);
        var directory = Directory.CreateTempSubdirectory("isoplane-native-debug-");
        try
        {
            var assembly = typeof(NativeDebugAdapter).Assembly;
            const string prefix = "Isoplane.Cli.Debugging.";
            foreach (var name in assembly.GetManifestResourceNames().Where(name => name.StartsWith(prefix, StringComparison.Ordinal) && name.EndsWith(".py", StringComparison.Ordinal)))
            {
                await using var resource = assembly.GetManifestResourceStream(name)
                    ?? throw new InvalidOperationException("The CLI is missing its NativeAOT debugger support.");
                await using var script = File.Create(Path.Combine(directory.FullName, name[prefix.Length..]));
                await resource.CopyToAsync(script, cancellation);
            }
            return await RunOwnedAsync(debugger, directory.FullName, cancellation);
        }
        finally { directory.Delete(recursive: true); }
    }

    private static async Task<int> RunOwnedAsync(NativeDebuggerToolchain debugger, string directory, CancellationToken cancellation)
    {
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        var launchEnvironment = System.Environment.GetEnvironmentVariables().Cast<System.Collections.DictionaryEntry>()
            .ToDictionary(entry => (string)entry.Key, entry => (string)entry.Value!);
        launchEnvironment[SessionDirectoryVariable] = directory;
        if (OperatingSystem.IsWindows()) launchEnvironment["_NO_DEBUG_HEAP"] = "1";
        var environment = new Dictionary<string, string>(launchEnvironment);
        foreach (var (name, value) in debugger.Environment) environment[name] = value;
        using var process = Processes.Start(debugger.Executable, [],
            Environment.CurrentDirectory, captureOutput: true,
            environment: environment);
        var relay = Isoplane.Debugging.NativeDebugSession.RunAsync(Console.OpenStandardInput(), Console.OpenStandardOutput(),
            process.StandardInput.BaseStream, process.StandardOutput.BaseStream, lifetime.Token, launchEnvironment);
        var exited = process.WaitForExitAsync(lifetime.Token);
        try
        {
            var completed = await Task.WhenAny(relay, exited);
            await completed;
            if (completed == relay)
            {
                await relay;
                process.StandardInput.Close();
            }
            await exited;
            await relay;
            return process.ExitCode;
        }
        finally
        {
            lifetime.Cancel();
            await Processes.StopAsync(process);
            try { await relay; }
            catch (OperationCanceledException) when (lifetime.IsCancellationRequested) { }
        }
    }

}
