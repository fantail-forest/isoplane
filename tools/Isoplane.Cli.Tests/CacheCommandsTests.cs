using Isoplane.Native;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class CacheCommandsTests
{
    private static (int Exit, string Output) Run(Func<string, TextWriter, int> command, string directory)
    {
        using var output = new StringWriter();
        return (command(directory, output), output.ToString());
    }

    private static async Task CompileRuntimeAsync(SessionFixture fixture)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var ready = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        await using var session = new LocalSession(SessionFixture.Module, fixture.Options, message =>
        {
            if (message["event"]?.GetValue<string>() == "ready") ready.TrySetResult();
        });
        await ready.Task.WaitAsync(deadline.Token);
    }

    [Fact]
    public void info_reports_a_cache_no_session_has_filled_without_creating_it()
    {
        var directory = Path.Combine(Path.GetTempPath(), "isoplane-cache-" + Guid.NewGuid().ToString("N"));
        var (exit, output) = Run(CacheCommands.Info, directory);
        Assert.Equal(0, exit);
        Assert.Contains(directory, output, StringComparison.Ordinal);
        Assert.Contains("Empty.", output, StringComparison.Ordinal);
        Assert.False(Directory.Exists(directory), "Reporting a cache must not create it.");
    }

    [Fact]
    public async Task clean_removes_the_runtime_a_session_compiled_and_info_reports_it_before_and_after()
    {
        using var fixture = new SessionFixture();
        await CompileRuntimeAsync(fixture);

        var before = Run(CacheCommands.Info, fixture.RuntimeCache);
        Assert.Equal(0, before.Exit);
        Assert.Contains("1 compiled runtime,", before.Output, StringComparison.Ordinal);
        Assert.Contains("last used", before.Output, StringComparison.Ordinal);

        var cleaned = Run(CacheCommands.Clean, fixture.RuntimeCache);
        Assert.Equal(0, cleaned.Exit);
        Assert.StartsWith("Removed ", cleaned.Output, StringComparison.Ordinal);
        Assert.Empty(Directory.GetFiles(fixture.RuntimeCache, "*.cwasm", SearchOption.AllDirectories));

        Assert.Contains("Empty.", Run(CacheCommands.Info, fixture.RuntimeCache).Output, StringComparison.Ordinal);
    }
}
