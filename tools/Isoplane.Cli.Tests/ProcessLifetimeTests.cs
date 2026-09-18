using System.Diagnostics;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class ProcessLifetimeTests
{
    [Theory]
    [InlineData("-TERM")]
    [InlineData("-INT")]
    public async Task stopping_the_cli_cleans_up_its_children_and_exits_successfully(string signal)
    {
        using var fixture = new ProcessFixture();
        using var cli = fixture.StartCli("sleep 300 & echo $! > child.pid; wait");
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
        using var cli = fixture.StartCli("exit 23");
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
        var result = await Processes.RunAsync("sh", ["-c", "sleep 300 & echo $! > child.pid; exit 0"],
            fixture.Directory, fixture.Cancellation, redirectOutput: true);
        Assert.True(result == 125, "Leaving live descendants must produce an actionable non-success result rather than hanging output collection.");
        await fixture.AssertChildStoppedAsync();
    }

    [Fact]
    public async Task a_successful_command_waits_for_its_children_to_finish_cleanup()
    {
        using var fixture = new ProcessFixture();
        var start = new ProcessStartInfo("sh")
        {
            WorkingDirectory = fixture.Directory,
            RedirectStandardOutput = true,
            UseShellExecute = false,
        };
        start.ArgumentList.Add("-c");
        start.ArgumentList.Add("exec 3<&0; sh -c 'echo ready; read release <&3' & echo $! > child.pid; exit 0");
        using var process = Isoplane.Processes.OwnedProcess.Start(start, TimeSpan.FromSeconds(30));
        Assert.True(await process.StandardOutput.ReadLineAsync(fixture.Cancellation) == "ready",
            "The child must hold its cleanup gate before its parent exits.");
        var statPath = $"/proc/{process.Id}/stat";
        while (true)
        {
            Assert.True(File.Exists(statPath), "The leader must remain unreaped while its child is finishing cleanup.");
            var stat = await File.ReadAllTextAsync(statPath, fixture.Cancellation);
            if (stat[(stat.LastIndexOf(')') + 2)..].StartsWith("Z ", StringComparison.Ordinal)) break;
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
        var run = Processes.RunAsync("sh", ["-c", "sleep 300 & echo $! > child.pid; wait"],
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

        internal Process StartCli(string command)
        {
            foreach (var file in System.IO.Directory.GetFiles(AppContext.BaseDirectory))
                File.Copy(file, Path.Combine(Directory, Path.GetFileName(file)));
            var runtimeDirectory = Path.Combine(Directory, "runtime");
            System.IO.Directory.CreateDirectory(runtimeDirectory);
            File.Copy(Path.Combine(AppContext.BaseDirectory, "runtime", "libisoplane_native.so"), Path.Combine(runtimeDirectory, "libisoplane_native.so"));
            var start = new ProcessStartInfo("dotnet") { UseShellExecute = false, WorkingDirectory = Directory };
            start.ArgumentList.Add(Path.Combine(Directory, Path.GetFileName(typeof(OwnedChild).Assembly.Location)));
            start.ArgumentList.Add("--run-process");
            start.ArgumentList.Add(command);
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
                if (File.Exists(path) && int.TryParse(await File.ReadAllTextAsync(path, Cancellation), out var pid))
                    return pid;
                await Task.Delay(10, Cancellation);
            }
        }

        internal async Task AssertChildStoppedAsync()
        {
            var pid = await WaitForChildAsync();
            var statPath = $"/proc/{pid}/stat";
            while (File.Exists(statPath))
            {
                string stat;
                try { stat = await File.ReadAllTextAsync(statPath, Cancellation); }
                catch (FileNotFoundException) { return; }
                if (stat[(stat.LastIndexOf(')') + 2)..].StartsWith("Z ", StringComparison.Ordinal))
                    return;
                await Task.Delay(10, Cancellation);
            }
        }

        public void Dispose()
        {
            deadline.Dispose();
            System.IO.Directory.Delete(Directory, recursive: true);
        }
    }
}
