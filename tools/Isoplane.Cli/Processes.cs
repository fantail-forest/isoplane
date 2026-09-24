using System.Diagnostics;
using Isoplane.Processes;

namespace Isoplane.Cli;

internal static class Processes
{
    internal static OwnedProcess Start(string executable, IEnumerable<string> arguments, string directory, bool captureOutput = false, bool captureErrors = false)
    {
        var start = new ProcessStartInfo(executable) { WorkingDirectory = directory, UseShellExecute = false, RedirectStandardOutput = captureOutput, RedirectStandardError = captureErrors };
        foreach (var argument in arguments)
            start.ArgumentList.Add(argument);
        start.Environment["MSBUILDDISABLENODEREUSE"] = "1";
        start.Environment["DOTNET_CLI_USE_MSBUILD_SERVER"] = "0";
        Toolchain.Apply(start);
        start.Environment["ISOPLANE_CLI_ASSEMBLY"] = typeof(Processes).Assembly.Location;
        var cleanupGrace = executable == "dotnet" ? TimeSpan.FromSeconds(1) : TimeSpan.Zero;
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

    internal static async Task CheckAsync(string executable, IEnumerable<string> arguments, string directory, CancellationToken cancellation)
    {
        using var process = Start(executable, arguments, directory, captureOutput: true, captureErrors: true);
        var output = process.StandardOutput.ReadToEndAsync(cancellation);
        var errors = process.StandardError.ReadToEndAsync(cancellation);
        try
        {
            await process.WaitForExitAsync(cancellation);
            var details = string.Join(Environment.NewLine, new[] { await output, await errors }
                .Where(value => !string.IsNullOrWhiteSpace(value)).Select(value => value.Trim()));
            if (process.ExitCode != 0)
                throw new InvalidOperationException($"{Path.GetFileName(executable)} cannot run (exit {process.ExitCode}); check its reported native dependencies.{Environment.NewLine}{details}");
        }
        finally
        {
            await StopAsync(process);
            await Task.WhenAll(output, errors);
        }
    }

    internal static Task StopAsync(OwnedProcess process) => process.StopAsync(TimeSpan.FromSeconds(10));
}
