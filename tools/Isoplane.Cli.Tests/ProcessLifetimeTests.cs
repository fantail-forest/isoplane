using System.Diagnostics;
using Xunit;

namespace Wisp.Cli.Tests;

public sealed class ProcessLifetimeTests
{
    [Fact]
    public async Task a_command_exiting_with_pipe_holding_descendants_is_cleaned_up_and_reported()
    {
        using var fixture = new ProcessFixture();
        var result = await Processes.RunAsync("sh", ["-c", "sleep 300 & echo $! > child.pid; exit 0"],
            fixture.Directory, fixture.Runtime, fixture.Cancellation, redirectOutput: true);
        Assert.True(result == 125, "Leaving live descendants must produce an actionable non-success result rather than hanging output collection.");
        await fixture.AssertChildStoppedAsync();
    }

    [Fact]
    public async Task killing_the_outer_cli_owner_also_terminates_its_supervised_descendants()
    {
        using var fixture = new ProcessFixture();
        var start = new ProcessStartInfo("dotnet") { UseShellExecute = false, WorkingDirectory = fixture.Directory };
        foreach (var argument in new[] { typeof(OwnedChild).Assembly.Location, "--nested-owner", fixture.Directory })
            start.ArgumentList.Add(argument);
        using var outer = Process.Start(start) ?? throw new InvalidOperationException("Could not start the outer CLI owner.");
        try
        {
            await fixture.WaitForChildAsync();
            outer.Kill();
            await outer.WaitForExitAsync(fixture.Cancellation);
            await fixture.AssertChildStoppedAsync();
        }
        finally
        {
            if (!outer.HasExited)
            {
                outer.Kill();
                await outer.WaitForExitAsync(fixture.Cancellation);
            }
        }
    }

    [Fact]
    public async Task cancelling_a_command_terminates_descendants_before_returning_to_its_caller()
    {
        using var fixture = new ProcessFixture();
        using var cancellation = CancellationTokenSource.CreateLinkedTokenSource(fixture.Cancellation);
        var run = Processes.RunAsync("sh", ["-c", "sleep 300 & echo $! > child.pid; wait"],
            fixture.Directory, fixture.Runtime, cancellation.Token, redirectOutput: true);
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
        internal string Directory { get; } = System.IO.Directory.CreateTempSubdirectory("wisp-process-test-").FullName;
        internal string Runtime => Path.Combine(AppContext.BaseDirectory, "runtime", "wisp");
        internal CancellationToken Cancellation => deadline.Token;

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
