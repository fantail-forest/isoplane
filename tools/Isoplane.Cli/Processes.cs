using System.Diagnostics;

namespace Wisp.Cli;

internal static class Processes
{
    internal static Process Start(string executable, IEnumerable<string> arguments, string directory, string runtime, bool captureOutput = false)
    {
        var start = new ProcessStartInfo(executable) { WorkingDirectory = directory, UseShellExecute = false, RedirectStandardOutput = captureOutput };
        foreach (var argument in arguments)
            start.ArgumentList.Add(argument);
        start.Environment["WISP_CLI"] = runtime;
        return Process.Start(start) ?? throw new InvalidOperationException($"Could not start {executable}.");
    }

    internal static async Task<int> RunAsync(string executable, IEnumerable<string> arguments, string directory, string runtime, CancellationToken cancellation)
    {
        using var process = Start(executable, arguments, directory, runtime);
        try
        {
            await process.WaitForExitAsync(cancellation);
            return process.ExitCode;
        }
        finally
        {
            await StopAsync(process);
        }
    }

    internal static async Task StopAsync(Process process)
    {
        if (!process.HasExited)
        {
            try { process.Kill(entireProcessTree: true); }
            catch (InvalidOperationException) when (process.HasExited)
            {
                await process.WaitForExitAsync();
                return;
            }
        }
        await process.WaitForExitAsync();
    }
}
