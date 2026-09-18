using Xunit;

namespace Wisp.Cli.Tests;

public sealed class ShellEnvironmentTests : IDisposable
{
    private readonly string userHome = Directory.CreateTempSubdirectory("isoplane-shell-").FullName;

    public void Dispose() => Directory.Delete(userHome, recursive: true);

    [Fact]
    public void The_default_home_is_named_through_HOME_and_puts_the_sdk_then_the_tools_directory_first_on_path()
    {
        var script = ShellEnvironment.Script(Path.Combine(userHome, ".isoplane"), userHome);
        Assert.DoesNotContain(userHome, script);
        Assert.DoesNotContain("ISOPLANE_HOME", script);
        var exports = script.Split('\n').Where(line => line.Contains("export PATH", StringComparison.Ordinal)).ToArray();
        Assert.Equal(2, exports.Length);
        Assert.Contains("$HOME/.dotnet/tools:$PATH", exports[0]);
        Assert.Contains("$HOME/.isoplane/dotnet:$PATH", exports[1]);
        Assert.Equal($". \"$HOME/.isoplane/env\" {ShellEnvironment.Marker}", ShellEnvironment.SourceLine(Path.Combine(userHome, ".isoplane"), userHome));
    }

    [Fact]
    public void A_configured_home_is_exported_for_msbuild_and_the_cli()
    {
        var home = Path.Combine(userHome, "sdk", "isoplane");
        var script = ShellEnvironment.Script(home, userHome);
        Assert.Contains($"export ISOPLANE_HOME=\"{home}\"", script);
        Assert.Contains($"{home}/dotnet:$PATH", script);
    }

    [Fact]
    public void The_source_line_is_added_once_and_removed_cleanly()
    {
        var profile = Path.Combine(userHome, ".profile");
        File.WriteAllText(profile, "export A=1");
        var line = ShellEnvironment.SourceLine(Path.Combine(userHome, ".isoplane"), userHome);
        Assert.True(ShellEnvironment.AddLine(profile, line), "the first call adds the line");
        Assert.False(ShellEnvironment.AddLine(profile, line), "a present line is not duplicated");
        Assert.Equal($"export A=1\n{line}\n", File.ReadAllText(profile));
        Assert.True(ShellEnvironment.RemoveLine(profile, line), "the line is removed");
        Assert.Equal("export A=1\n", File.ReadAllText(profile));
        Assert.False(ShellEnvironment.RemoveLine(profile, line), "removing an absent line changes nothing");
    }

    [Fact]
    public void Startup_files_are_the_login_profile_plus_the_interactive_files_that_exist()
    {
        File.WriteAllText(Path.Combine(userHome, ".bashrc"), "");
        var files = ShellEnvironment.StartupFiles(userHome);
        Assert.Equal([Path.Combine(userHome, ".profile"), Path.Combine(userHome, ".bashrc")], files);
    }
}
