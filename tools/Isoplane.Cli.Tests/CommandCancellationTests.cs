using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class CommandCancellationTests
{
    [Theory]
    [InlineData("setup")]
    [InlineData("uninstall-sdk")]
    public void Cancelled_sdk_changes_report_failure_and_a_retry_command(string command)
    {
        using var error = new StringWriter();
        var status = CommandCancellation.Report(command, error);
        Assert.True(status == 130, "Cancelled SDK changes must not report success.");
        Assert.True(error.ToString().Contains($"Run isoplane {command} again", StringComparison.Ordinal),
            "Cancellation must tell the user how to retry the incomplete operation.");
    }

    [Theory]
    [InlineData("dev", 0)]
    [InlineData("bundle", 130)]
    public void Other_commands_preserve_their_shutdown_status(string command, int expected)
    {
        using var error = new StringWriter();
        Assert.True(CommandCancellation.Report(command, error) == expected,
            "Development shutdown and interrupted build commands must preserve their exit semantics.");
    }
}
