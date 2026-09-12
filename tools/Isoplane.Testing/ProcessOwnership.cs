using System.Diagnostics;

namespace Wisp.Testing;

internal static class ProcessOwnership
{
    internal static async Task<string> ResolveAsync(string executable, TimeSpan timeout, CancellationToken cancellation)
    {
        var start = new ProcessStartInfo(executable)
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        start.ArgumentList.Add("process-owner-path");
        try
        {
            using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not query the installed process owner.");
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            deadline.CancelAfter(timeout);
            var output = process.StandardOutput.ReadToEndAsync(deadline.Token);
            var errors = process.StandardError.ReadToEndAsync(deadline.Token);
            try
            {
                await process.WaitForExitAsync(deadline.Token);
                var path = (await output).Trim();
                var detail = await errors;
                if (process.ExitCode != 0 || !Path.IsPathFullyQualified(path) || !File.Exists(path))
                    throw new InvalidOperationException($"Installed CLI '{executable}' did not provide its process owner; reinstall the matching CLI package. {detail}");
                return path;
            }
            finally
            {
                if (!process.HasExited)
                {
                    process.Kill(entireProcessTree: true);
                    await process.WaitForExitAsync();
                }
            }
        }
        catch (System.ComponentModel.Win32Exception error)
        {
            throw new InvalidOperationException($"Installed CLI '{executable}' is unavailable; install Isoplane.Cli or set ApplicationOptions.CliPath.", error);
        }
    }
}
