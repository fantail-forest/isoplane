namespace Isoplane.Cli;

internal static class NativeDebugAdapter
{
    internal const string SessionDirectoryVariable = "ISOPLANE_NATIVE_DEBUG_DIRECTORY";

    internal static async Task<int> RunAsync(CancellationToken cancellation)
    {
        await Processes.CheckAsync("lldb-dap", ["--help"], Environment.CurrentDirectory, cancellation);
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
            return await RunOwnedAsync(directory.FullName, cancellation);
        }
        finally { directory.Delete(recursive: true); }
    }

    private static async Task<int> RunOwnedAsync(string directory, CancellationToken cancellation)
    {
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        using var process = Processes.Start("lldb-dap", [],
            Environment.CurrentDirectory, captureOutput: true,
            environment: new Dictionary<string, string> { [SessionDirectoryVariable] = directory });
        var relay = Isoplane.Debugging.NativeDebugSession.RunAsync(Console.OpenStandardInput(), Console.OpenStandardOutput(),
            process.StandardInput.BaseStream, process.StandardOutput.BaseStream, lifetime.Token);
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
