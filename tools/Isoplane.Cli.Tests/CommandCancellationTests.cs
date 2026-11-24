using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class CommandCancellationTests
{
    private static Task<int> Cancelled(CancellationToken _) => throw new OperationCanceledException(new CancellationToken(canceled: true));

    [Fact]
    public async Task A_cancelled_change_to_the_machine_says_how_to_retry_and_ends_in_its_cancellation()
    {
        using var error = new StringWriter();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CommandCancellation.RunOrRetryAsync("setup", Cancelled, error));
        Assert.True(error.ToString().Contains("Run isoplane setup again", StringComparison.Ordinal),
            "Cancellation must tell the user how to retry the incomplete operation.");
    }

    [Fact]
    public async Task A_command_that_cancellation_stops_as_intended_ends_with_status_zero()
    {
        Assert.Equal(0, await CommandCancellation.RunUntilStoppedAsync(Cancelled, CommandTelemetry.Begin(["complete"])));
    }

    [Fact]
    public async Task Another_cancelled_command_ends_in_its_cancellation()
    {
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CommandCancellation.RunAsync(Cancelled));
    }
}
