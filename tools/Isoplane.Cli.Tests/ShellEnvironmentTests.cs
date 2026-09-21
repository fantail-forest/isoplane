using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class ShellEnvironmentTests
{
    [Fact]
    public void Removing_a_recorded_shell_entry_preserves_other_lines()
    {
        using var fixture = new ToolchainBundle();
        var profile = Path.Combine(fixture.UserHome, ".profile");
        const string entry = ". \"$HOME/.isoplane/env\" # isoplane setup";
        File.WriteAllText(profile, "# user profile\n" + entry + "\n");
        Assert.True(ShellEnvironment.RemoveLine(profile, entry), "the recorded setup line must be removed");
        Assert.True(File.ReadAllText(profile) == "# user profile\n", "removal must preserve user configuration");
        Assert.False(ShellEnvironment.RemoveLine(profile, entry), "removing an absent entry must leave the profile unchanged");
    }
}
