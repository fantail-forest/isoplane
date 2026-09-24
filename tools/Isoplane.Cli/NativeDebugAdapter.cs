namespace Isoplane.Cli;

internal static class NativeDebugAdapter
{
    internal const string SessionDirectoryVariable = "ISOPLANE_NATIVE_DEBUG_DIRECTORY";

    internal static async Task<int> RunAsync(CancellationToken cancellation)
    {
        if (!OperatingSystem.IsLinux())
            throw new InvalidOperationException("NativeAOT editor debugging currently supports Linux with GDB 16 or later and Python DAP support.");
        await Processes.CheckAsync("gdb", ["--batch", "--nx", "-ex",
            "python import gdb, importlib.util; assert int(gdb.VERSION.split('.')[0]) >= 16 and importlib.util.find_spec('gdb.dap') is not None, 'NativeAOT debugging requires GDB 16 or later with Python DAP support'"],
            Environment.CurrentDirectory, cancellation);
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
        using var process = Processes.Start("gdb", ["--quiet", "--nx", "--interpreter=dap",
            "-iex", "set pagination off", "-iex", "set debuginfod enabled off",
            "-iex", "set breakpoint pending on", "-iex", "set print thread-events off",
            "-iex", "python import os, runpy; runpy.run_path(os.path.join(os.environ['ISOPLANE_NATIVE_DEBUG_DIRECTORY'], 'isoplane_gdb.py'))",
            "-iex", "handle SIGSEGV nostop noprint pass", "-iex", "handle SIGILL nostop noprint pass",
            "-iex", "handle SIG34 nostop noprint pass"],
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
