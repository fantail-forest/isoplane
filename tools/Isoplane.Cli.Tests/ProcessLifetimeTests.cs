using Isoplane.TestSupport;
using System.Diagnostics;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class ProcessLifetimeTests
{
    [PosixTheory]
    [InlineData("-TERM")]
    [InlineData("-INT")]
    public async Task stopping_the_cli_cleans_up_its_children_and_exits_successfully(string signal)
    {
        using var fixture = new ProcessFixture();
        using var cli = fixture.StartCli("wait-for-child");
        try
        {
            await fixture.WaitForChildAsync();
            using var request = Process.Start("kill", [signal, cli.Id.ToString()])
                ?? throw new InvalidOperationException("Could not signal the CLI.");
            await request.WaitForExitAsync(fixture.Cancellation);
            Assert.True(request.ExitCode == 0, "The running CLI must receive the shutdown signal.");
            await cli.WaitForExitAsync(fixture.Cancellation);
            Assert.True(cli.ExitCode == 0, "An intentional CLI shutdown must not appear as a failed application in dotnet watch.");
            await fixture.AssertChildStoppedAsync();
        }
        finally
        {
            await fixture.StopCliAsync(cli);
        }
    }

    [Fact]
    public async Task the_cli_preserves_a_command_failure_exit_code()
    {
        using var fixture = new ProcessFixture();
        using var cli = fixture.StartCli("exit", "23");
        try
        {
            await cli.WaitForExitAsync(fixture.Cancellation);
            Assert.True(cli.ExitCode == 23, "A command failure must remain visible to dotnet watch and other CLI callers.");
        }
        finally
        {
            await fixture.StopCliAsync(cli);
        }
    }

    [Fact]
    public async Task a_command_exiting_with_pipe_holding_descendants_is_cleaned_up_and_reported()
    {
        using var fixture = new ProcessFixture();
        var result = await Processes.RunAsync(fixture.Executable, ["orphan"],
            fixture.Directory, fixture.Cancellation, redirectOutput: true);
        Assert.True(result == 125, "Leaving live descendants must produce an actionable non-success result rather than hanging output collection.");
        await fixture.AssertChildStoppedAsync();
    }

    [Fact]
    public async Task a_successful_command_waits_for_its_children_to_finish_cleanup()
    {
        using var fixture = new ProcessFixture();
        var start = new ProcessStartInfo(fixture.Executable)
        {
            WorkingDirectory = fixture.Directory,
            RedirectStandardOutput = true,
            UseShellExecute = false,
        };
        start.ArgumentList.Add("cleanup-child");
        using var process = Isoplane.Processes.OwnedProcess.Start(start, TimeSpan.FromSeconds(30));
        Assert.True(await process.StandardOutput.ReadLineAsync(fixture.Cancellation) == "ready",
            "The child must hold its cleanup gate before its parent exits.");
        while (true)
        {
            var state = await fixture.ProcessStateAsync(process.Id);
            Assert.True(OperatingSystem.IsWindows() || state is not null,
                "The POSIX group leader must remain unreaped while its child is finishing cleanup.");
            if (state is null || state.StartsWith("Z", StringComparison.Ordinal)) break;
            await Task.Delay(10, fixture.Cancellation);
        }
        Assert.False(process.HasExited, "The command must await the live child's cleanup after the leader exits.");
        await process.StandardInput.WriteLineAsync("finish".AsMemory(), fixture.Cancellation);
        await process.WaitForExitAsync(fixture.Cancellation);
        Assert.True(process.ExitCode == 0, "Children completing cleanup must preserve a successful command's exit code.");
        await fixture.AssertChildStoppedAsync();
    }

    [Fact]
    public async Task cancelling_a_command_terminates_descendants_before_returning_to_its_caller()
    {
        using var fixture = new ProcessFixture();
        using var cancellation = CancellationTokenSource.CreateLinkedTokenSource(fixture.Cancellation);
        var run = Processes.RunAsync(fixture.Executable, ["wait-for-child"],
            fixture.Directory, cancellation.Token, redirectOutput: true);
        try
        {
            await fixture.WaitForChildAsync();
        }
        finally
        {
            cancellation.Cancel();
        }
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => run);
        await fixture.AssertChildStoppedAsync();
    }

    private sealed class ProcessFixture : IDisposable
    {
        private readonly CancellationTokenSource deadline = new(TimeSpan.FromSeconds(30));
        internal string Directory { get; } = System.IO.Directory.CreateTempSubdirectory("isoplane-child-test-").FullName;
        internal CancellationToken Cancellation => deadline.Token;
        internal string Executable => Environment.GetEnvironmentVariable("ISOPLANE_TEST_PROCESS_FIXTURE")
            ?? throw new InvalidOperationException("Run task test:managed to prepare the process fixture.");

        internal Process StartCli(params string[] command)
        {
            var start = new ProcessStartInfo("dotnet") { UseShellExecute = false, WorkingDirectory = Directory };
            start.ArgumentList.Add(typeof(OwnedChild).Assembly.Location);
            start.ArgumentList.Add("--run-process");
            foreach (var argument in command) start.ArgumentList.Add(argument);
            start.Environment["ISOPLANE_HOME"] = Path.Combine(Directory, "home");
            return Process.Start(start) ?? throw new InvalidOperationException("Could not start the CLI.");
        }

        internal async Task StopCliAsync(Process cli)
        {
            if (!cli.HasExited)
            {
                cli.Kill(entireProcessTree: true);
                await cli.WaitForExitAsync(Cancellation);
            }
        }

        internal async Task<int> WaitForChildAsync()
        {
            var path = Path.Combine(Directory, "child.pid");
            while (true)
            {
                if (File.Exists(path))
                {
                    // The atomic rename can still hold delete access when the new name becomes visible.
                    using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
                    using var reader = new StreamReader(stream);
                    if (int.TryParse(await reader.ReadToEndAsync(Cancellation), out var pid)) return pid;
                }
                await Task.Delay(10, Cancellation);
            }
        }

        internal async Task AssertChildStoppedAsync()
        {
            var pid = await WaitForChildAsync();
            while (await ProcessStateAsync(pid) is { } state && !state.StartsWith("Z", StringComparison.Ordinal))
                await Task.Delay(10, Cancellation);
        }

        internal async Task<string?> ProcessStateAsync(int pid)
        {
            if (OperatingSystem.IsWindows())
            {
                try
                {
                    using var child = Process.GetProcessById(pid);
                    return child.HasExited ? null : "R";
                }
                catch (ArgumentException) { return null; }
            }
            var start = new ProcessStartInfo("ps") { RedirectStandardOutput = true, RedirectStandardError = true, UseShellExecute = false };
            foreach (var argument in new[] { "-o", "stat=", "-p", pid.ToString() })
                start.ArgumentList.Add(argument);
            using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not inspect the owned process.");
            var output = process.StandardOutput.ReadToEndAsync(Cancellation);
            var errors = process.StandardError.ReadToEndAsync(Cancellation);
            await process.WaitForExitAsync(Cancellation);
            Assert.True(process.ExitCode is 0 or 1, "Process-state inspection must succeed or report an absent PID: " + await errors);
            return process.ExitCode == 1 ? null : (await output).Trim();
        }

        public void Dispose()
        {
            deadline.Dispose();
            System.IO.Directory.Delete(Directory, recursive: true);
        }
    }
}
