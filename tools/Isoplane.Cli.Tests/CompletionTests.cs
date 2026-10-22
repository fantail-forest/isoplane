using System.Diagnostics;
using Isoplane.Cli;
using Isoplane.TestSupport;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class CompletionTests
{
    [Theory]
    [InlineData("isoplane dev --launch-", "--launch-profile")]
    [InlineData("isoplane completions p", "powershell")]
    public void suggestions_come_from_the_command_parser(string line, string expected)
    {
        Assert.Contains(expected, ShellCompletions.Candidates(line));
    }

    [Fact]
    public void suggestions_respect_command_scope_and_hide_internal_commands_and_options()
    {
        Assert.DoesNotContain("--debug-launch-endpoint", ShellCompletions.Candidates("isoplane dev --"));
        Assert.DoesNotContain("complete", ShellCompletions.Candidates("isoplane "));
    }

    [Theory]
    [InlineData("bash", "complete -o default -F _isoplane_complete isoplane")]
    [InlineData("zsh", "compdef _isoplane_complete isoplane")]
    [InlineData("fish", "complete -c isoplane")]
    [InlineData("powershell", "Register-ArgumentCompleter -Native -CommandName isoplane")]
    public async Task the_cli_prints_shell_registration_without_reading_configuration(string shell, string registration)
    {
        var result = await RunAsync("completions", shell);
        Assert.Equal(0, result.ExitCode);
        Assert.Contains(registration, result.Output);
        Assert.Equal("", result.Error);
    }

    [Fact]
    public async Task the_cli_returns_only_candidates_without_initializing_telemetry_or_configuration()
    {
        var result = await RunAsync("complete", "isoplane bundle --out");
        Assert.Equal(0, result.ExitCode);
        Assert.Equal("--output", result.Output.Trim());
        Assert.Equal("", result.Error);
    }

    [Theory]
    [InlineData("completions")]
    [InlineData("completions", "unknown")]
    [InlineData("completions", "bash", "extra")]
    [InlineData("complete")]
    public async Task invalid_completion_requests_fail_with_no_script(params string[] args)
    {
        var result = await RunAsync(args);
        Assert.NotEqual(0, result.ExitCode);
        Assert.Equal("", result.Output);
        Assert.NotEmpty(result.Error);
    }

    private static async Task<(int ExitCode, string Output, string Error)> RunAsync(params string[] args)
    {
        using var directory = new FileSystemTestDirectory("isoplane-completion-");
        var home = Path.Combine(directory.Path, "home");
        Directory.CreateDirectory(home);
        File.WriteAllText(Path.Combine(home, "config.json"), "invalid JSON");
        var start = new ProcessStartInfo("dotnet")
        {
            UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true,
            WorkingDirectory = directory.Path
        };
        start.Environment["ISOPLANE_HOME"] = home;
        start.Environment["ISOPLANE_TELEMETRY_DEBUG"] = "1";
        start.ArgumentList.Add(typeof(CliPaths).Assembly.Location);
        foreach (var argument in args) start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start the CLI.");
        var output = process.StandardOutput.ReadToEndAsync();
        var error = process.StandardError.ReadToEndAsync();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        try
        {
            await process.WaitForExitAsync(deadline.Token);
            return (process.ExitCode, await output, await error);
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
