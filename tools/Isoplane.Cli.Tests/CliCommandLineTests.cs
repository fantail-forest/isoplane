using Isoplane.TestSupport;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class CliCommandLineTests
{
    [Fact]
    public void development_parsing_preserves_aliases_paths_and_switches()
    {
        using var project = new FileSystemTestDirectory("isoplane-cli-project-");
        File.WriteAllText(Path.Combine(project.Path, "App.csproj"), "<Project />");
        var options = DevelopmentOptions.Parse(["--project", project.Path, "-c", "Release", "--port", "1234",
            "--no-watch", "--no-build", "--debug", "--debug-launch-endpoint", "127.0.0.1:9000",
            "--no-launch-profile", "--json", "--shutdown-on-stdin-close"]);
        Assert.Equal(project.Path, options.Directory);
        Assert.Equal("Release", options.Configuration);
        Assert.Equal((ushort)1234, options.Port);
        Assert.False(options.Watch);
        Assert.True(options.NoBuild);
        Assert.True(options.Debug);
        Assert.Equal("127.0.0.1:9000", options.DebugLaunchEndpoint?.ToString());
        Assert.False(options.UseLaunchProfile);
        Assert.True(options.Json);
        Assert.True(options.ShutdownOnInputClose);
    }

    [Theory]
    [InlineData("--no-build")]
    [InlineData("--port", "65536")]
    [InlineData("--launch-profile", "http", "--no-launch-profile")]
    [InlineData("--debug-launch-endpoint", "127.0.0.1:9000")]
    public void development_parsing_preserves_invalid_combination_checks(params string[] args)
    {
        using var project = new FileSystemTestDirectory("isoplane-cli-project-");
        File.WriteAllText(Path.Combine(project.Path, "App.csproj"), "<Project />");
        Assert.Throws<ArgumentException>(() => DevelopmentOptions.Parse(["--project", project.Path, .. args]));
    }

    [Fact]
    public void bundle_parsing_preserves_output_and_configuration()
    {
        var options = BundleOptions.Parse(["--project", ".", "--configuration", "Custom", "--output", "application.tar"]);
        Assert.Equal(Environment.CurrentDirectory, options.Directory);
        Assert.Equal("Custom", options.Configuration);
        Assert.Equal(Path.GetFullPath("application.tar"), options.Output);
        Assert.Throws<ArgumentException>(() => BundleOptions.Parse([]));
    }
}
