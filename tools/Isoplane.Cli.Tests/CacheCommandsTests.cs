using Isoplane.Native;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class CacheCommandsTests
{
    private static (int Exit, string Output) Run(string directory, params string[] arguments)
    {
        using var output = new StringWriter();
        return (CacheCommands.Run(arguments, directory, output), output.ToString());
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
        var (exit, output) = Run(directory, "cache", "info");
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

        var before = Run(fixture.RuntimeCache, "cache", "info");
        Assert.Equal(0, before.Exit);
        Assert.Contains("1 compiled runtime,", before.Output, StringComparison.Ordinal);
        Assert.Contains("last used", before.Output, StringComparison.Ordinal);

        var cleaned = Run(fixture.RuntimeCache, "cache", "clean");
        Assert.Equal(0, cleaned.Exit);
        Assert.StartsWith("Removed ", cleaned.Output, StringComparison.Ordinal);
        Assert.Empty(Directory.GetFiles(fixture.RuntimeCache, "*.cwasm", SearchOption.AllDirectories));

        Assert.Contains("Empty.", Run(fixture.RuntimeCache, "cache", "info").Output, StringComparison.Ordinal);
    }

    [Theory]
    [InlineData("cache")]
    [InlineData("cache", "purge")]
    [InlineData("cache", "clean", "--all")]
    public void an_unknown_cache_command_names_the_two_that_exist(params string[] arguments)
    {
        var error = Assert.Throws<ArgumentException>(() => Run(Path.GetTempPath(), arguments));
        Assert.Equal("Use isoplane cache info or isoplane cache clean.", error.Message);
    }
}
