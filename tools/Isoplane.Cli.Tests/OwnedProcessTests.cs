using System.Diagnostics;
using Isoplane.Processes;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class OwnedProcessTests
{
    private static ProcessStartInfo Command(string unix, string windows)
    {
        var start = new ProcessStartInfo(OperatingSystem.IsWindows() ? "cmd.exe" : "sh")
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        if (OperatingSystem.IsWindows())
        {
            start.ArgumentList.Add("/d");
            start.ArgumentList.Add("/c");
            start.ArgumentList.Add(windows);
        }
        else
        {
            start.ArgumentList.Add("-c");
            start.ArgumentList.Add(unix);
        }
        return start;
    }

    [Fact]
    public async Task captured_output_and_failure_status_reach_the_caller()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var process = OwnedProcess.Start(Command("echo output; echo error >&2; exit 23", "echo output & echo error >&2 & exit /b 23"), TimeSpan.Zero);
        var output = process.StandardOutput.ReadToEndAsync(deadline.Token);
        var errors = process.StandardError.ReadToEndAsync(deadline.Token);
        await process.WaitForExitAsync(deadline.Token);
        Assert.True((await output).Trim() == "output", "Captured stdout must reach the managed caller.");
        Assert.True((await errors).Trim() == "error", "Captured stderr must reach the managed caller.");
        Assert.True(process.ExitCode == 23, "The child failure must be preserved across native process ownership.");
    }

    [Fact]
    public async Task an_exited_parent_cannot_leave_pipe_holding_children_running()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var process = OwnedProcess.Start(Command("sleep 300 & exit 0", "start /b ping -n 300 127.0.0.1 & exit /b 0"), TimeSpan.Zero);
        var output = process.StandardOutput.ReadToEndAsync(deadline.Token);
        var errors = process.StandardError.ReadToEndAsync(deadline.Token);
        await process.WaitForExitAsync(deadline.Token);
        await Task.WhenAll(output, errors);
        Assert.True(process.ExitCode == 125, "Live descendants must be terminated and reported instead of surviving a successful command.");
    }

    [Fact]
    public async Task stopping_a_process_tree_closes_all_inherited_output_pipes()
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        using var process = OwnedProcess.Start(Command("sleep 300 & echo ready; wait", "start /b ping -n 300 127.0.0.1 >nul & echo ready & ping -n 300 127.0.0.1 >nul"), TimeSpan.Zero);
        Assert.True((await process.StandardOutput.ReadLineAsync(deadline.Token))?.Trim() == "ready", "The parent must create its child before cancellation.");
        var output = process.StandardOutput.ReadToEndAsync(deadline.Token);
        var errors = process.StandardError.ReadToEndAsync(deadline.Token);
        await process.StopAsync(TimeSpan.FromSeconds(20));
        await Task.WhenAll(output, errors);
        Assert.True(process.HasExited, "Cancellation must finish process-tree cleanup before returning.");
    }
}
