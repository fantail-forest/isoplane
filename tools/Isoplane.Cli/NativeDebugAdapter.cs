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
        try { return await RunOwnedAsync(directory.FullName, cancellation); }
        finally { directory.Delete(recursive: true); }
    }

    private static async Task<int> RunOwnedAsync(string directory, CancellationToken cancellation)
    {
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        using var process = Processes.Start("gdb", ["--quiet", "--nx", "--interpreter=dap",
            "-iex", "set pagination off", "-iex", "set debuginfod enabled off",
            "-iex", "set breakpoint pending on", "-iex", "set print thread-events off",
            "-iex", "handle SIGSEGV nostop noprint pass", "-iex", "handle SIGILL nostop noprint pass",
            "-iex", "handle SIG34 nostop noprint pass"],
            Environment.CurrentDirectory, captureOutput: true,
            environment: new Dictionary<string, string> { [SessionDirectoryVariable] = directory });
        var input = ForwardInputAsync(Console.OpenStandardInput(), process.StandardInput.BaseStream, lifetime.Token);
        var output = process.StandardOutput.BaseStream.CopyToAsync(Console.OpenStandardOutput(), lifetime.Token);
        var exited = process.WaitForExitAsync(lifetime.Token);
        try
        {
            var completed = await Task.WhenAny(input, output, exited);
            await completed;
            if (completed == input)
            {
                await input;
                process.StandardInput.Close();
            }
            await exited;
            await output;
            return process.ExitCode;
        }
        finally
        {
            lifetime.Cancel();
            await Processes.StopAsync(process);
            try { await Task.WhenAll(input, output); }
            catch (OperationCanceledException) when (lifetime.IsCancellationRequested) { }
        }
    }

    private static async Task ForwardInputAsync(Stream input, Stream output, CancellationToken cancellation)
    {
        var bytes = new byte[8192];
        while (true)
        {
            var count = await input.ReadAsync(bytes.AsMemory(), cancellation).AsTask().WaitAsync(cancellation);
            if (count == 0) return;
            await output.WriteAsync(bytes.AsMemory(0, count), cancellation);
            await output.FlushAsync(cancellation);
        }
    }
}
