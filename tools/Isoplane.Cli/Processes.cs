using System.Diagnostics;
using Isoplane.Processes;

namespace Isoplane.Cli;

internal static class Processes
{
    internal static OwnedProcess Start(string executable, IEnumerable<string> arguments, string directory, bool captureOutput = false, bool captureErrors = false, TimeSpan? shutdownGrace = null)
    {
        var start = new ProcessStartInfo(executable) { WorkingDirectory = directory, UseShellExecute = false, RedirectStandardOutput = captureOutput, RedirectStandardError = captureErrors };
        foreach (var argument in arguments)
            start.ArgumentList.Add(argument);
        start.Environment["MSBUILDDISABLENODEREUSE"] = "1";
        start.Environment["DOTNET_CLI_USE_MSBUILD_SERVER"] = "0";
        start.Environment["ISOPLANE_CLI_ASSEMBLY"] = typeof(Processes).Assembly.Location;
        var cleanupGrace = shutdownGrace ?? (executable == "dotnet" ? TimeSpan.FromSeconds(1) : TimeSpan.Zero);
        return OwnedProcess.Start(start, cleanupGrace);
    }

    internal static async Task<int> RunAsync(string executable, IEnumerable<string> arguments, string directory, CancellationToken cancellation, bool redirectOutput = false)
    {
        using var process = Start(executable, arguments, directory, captureOutput: redirectOutput);
        using var outputLifetime = new CancellationTokenSource();
        var output = redirectOutput ? process.StandardOutput.BaseStream.CopyToAsync(Console.OpenStandardError(), outputLifetime.Token) : Task.CompletedTask;
        try
        {
            await process.WaitForExitAsync(cancellation);
            return process.ExitCode;
        }
        finally
        {
            try
            {
                await StopAsync(process);
                await output;
            }
            finally
            {
                outputLifetime.Cancel();
            }
        }
    }

    internal static Task StopAsync(OwnedProcess process) => process.StopAsync(TimeSpan.FromSeconds(10));
}
