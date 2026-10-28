using System.Diagnostics;
using Isoplane.Cli;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class HelpTests
{
    [Theory]
    [InlineData("dev", "--help")]
    [InlineData("doctor", "-h")]
    [InlineData("setup", "--help")]
    [InlineData("completions", "--help")]
    [InlineData("cache", "clean", "--help")]
    [InlineData("bundle", "-h")]
    [InlineData("agent", "init", "--help")]
    public async Task Command_help_does_not_require_configuration_or_start_the_command(params string[] arguments)
    {
        var start = new ProcessStartInfo("dotnet")
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        start.ArgumentList.Add(typeof(CliPaths).Assembly.Location);
        foreach (var argument in arguments) start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start the CLI help command.");
        var output = process.StandardOutput.ReadToEndAsync();
        var errors = process.StandardError.ReadToEndAsync();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        try
        {
            await process.WaitForExitAsync(deadline.Token);
            Assert.True(process.ExitCode == 0, $"Command help must succeed without command arguments: {await errors}");
            Assert.True((await output).Contains($"isoplane {string.Join(' ', arguments[..^1])}", StringComparison.Ordinal),
                "Help must describe the requested command.");
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
}
