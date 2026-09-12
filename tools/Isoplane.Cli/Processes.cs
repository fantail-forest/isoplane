using System.Diagnostics;
using Wisp.Processes;

namespace Wisp.Cli;

internal static class Processes
{
    internal static OwnedProcess Start(string executable, IEnumerable<string> arguments, string directory, string runtime, bool captureOutput = false)
    {
        var start = new ProcessStartInfo(executable) { WorkingDirectory = directory, UseShellExecute = false, RedirectStandardOutput = captureOutput };
        foreach (var argument in arguments)
            start.ArgumentList.Add(argument);
        start.Environment["WISP_CLI"] = runtime;
        start.Environment["MSBUILDDISABLENODEREUSE"] = "1";
        start.Environment["DOTNET_CLI_USE_MSBUILD_SERVER"] = "0";
        Toolchain.Apply(start);
        var bundled = Path.Combine(AppContext.BaseDirectory, "runtime", "wisp-process");
        var supervisor = File.Exists(bundled) ? bundled : Path.Combine(Path.GetDirectoryName(Path.GetFullPath(runtime))!, "wisp-process");
        var cleanupGrace = executable == "dotnet" ? TimeSpan.FromSeconds(1) : TimeSpan.Zero;
        return OwnedProcess.Start(supervisor, start, cleanupGrace);
    }

    internal static async Task<int> RunAsync(string executable, IEnumerable<string> arguments, string directory, string runtime, CancellationToken cancellation, bool redirectOutput = false)
    {
        using var process = Start(executable, arguments, directory, runtime, captureOutput: redirectOutput);
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
