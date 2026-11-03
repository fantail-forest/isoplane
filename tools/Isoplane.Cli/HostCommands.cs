using System.CommandLine;

namespace Isoplane.Cli;

internal static class HostCommands
{
    internal static async Task<int> RunAsync(ParseResult parsed, CancellationToken cancellation)
    {
        var serving = parsed.CommandResult.Command.Name == "daemon";
        List<string> arguments = [serving ? "serve" : "deploy", "--config", parsed.GetValue<string>("--config")!];
        if (serving)
        {
            if (parsed.GetValue<bool>("--check")) arguments.Add("--check");
            else if (parsed.GetValue<bool>("--print-config")) arguments.Add("--print-config");
            else arguments.Add("--shutdown-on-stdin-close");
        }
        else arguments.AddRange(["--app", parsed.GetValue<string>("--app")!, "--bundle", parsed.GetValue<string>("--bundle")!]);

        var shutdown = serving ? TimeSpan.FromSeconds(180) : TimeSpan.FromSeconds(10);
        using var process = Processes.Start("isoplaned", arguments, Environment.CurrentDirectory, shutdownGrace: serving ? shutdown : TimeSpan.Zero);
        try
        {
            await process.WaitForExitAsync(cancellation);
            return process.ExitCode;
        }
        finally
        {
            await process.StopAsync(shutdown);
        }
    }
}
