using System.Text.Json;
using Xunit;

namespace Wisp.Cli.Tests;

public sealed class ReleaseTests
{
    private static readonly ReleaseIdentity Cli = new("0.1.0-preview.1", "0123456789abcdef0123456789abcdef01234567");

    [Fact]
    public async Task Setup_installs_the_toolchain_of_the_release_the_cli_was_built_from()
    {
        using var fixture = new ToolchainBundle();
        fixture.DescribeRelease(Cli.Version, Cli.SourceRevision);
        var installed = await Release.InstallAsync(fixture.Release, Cli, fixture.Home, CancellationToken.None);
        Assert.True(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None) == installed, "a matching release must leave a verified toolchain selected");
    }

    [Fact]
    public async Task A_release_of_another_version_is_refused_with_the_command_that_installs_its_cli()
    {
        using var fixture = new ToolchainBundle();
        fixture.DescribeRelease("0.1.0-preview.2", Cli.SourceRevision);
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Release.InstallAsync(fixture.Release, Cli, fixture.Home, CancellationToken.None));
        Assert.Contains("dotnet tool update --global Isoplane.Cli --version 0.1.0-preview.2", error.Message);
        Assert.False(Directory.Exists(fixture.Home), "a toolchain from another release must not be installed beside this CLI");
    }

    [Fact]
    public async Task A_release_assembled_from_another_revision_of_the_same_version_is_refused()
    {
        using var fixture = new ToolchainBundle();
        fixture.DescribeRelease(Cli.Version, "fedcba9876543210fedcba9876543210fedcba98");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Release.InstallAsync(fixture.Release, Cli, fixture.Home, CancellationToken.None));
        Assert.Contains("revision fedcba987654", error.Message);
        Assert.False(Directory.Exists(fixture.Home), "components of two builds of one version must not be mixed");
    }

    [Fact]
    public async Task A_toolchain_the_release_manifest_does_not_describe_is_refused()
    {
        using var fixture = new ToolchainBundle();
        fixture.DescribeRelease(Cli.Version, Cli.SourceRevision);
        var path = Path.Combine(fixture.Source, "toolchain.json");
        await File.WriteAllTextAsync(path, await File.ReadAllTextAsync(path) + "\n");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Release.InstallAsync(fixture.Release, Cli, fixture.Home, CancellationToken.None));
        Assert.Contains("incomplete or altered", error.Message);
        Assert.False(Directory.Exists(fixture.Home), "an altered toolchain must not be installed");
    }

    [Fact]
    public async Task A_toolchain_directory_alone_is_not_a_release()
    {
        using var fixture = new ToolchainBundle();
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Release.InstallAsync(fixture.Source, Cli, fixture.Home, CancellationToken.None));
        Assert.Contains(Release.ManifestName, error.Message);
    }

    [Fact]
    public async Task A_manifest_missing_the_release_identity_is_refused()
    {
        using var fixture = new ToolchainBundle();
        await File.WriteAllTextAsync(Path.Combine(fixture.Release, Release.ManifestName), JsonSerializer.Serialize(new { packageVersion = Cli.Version }));
        await Assert.ThrowsAsync<InvalidOperationException>(() => Release.InstallAsync(fixture.Release, Cli, fixture.Home, CancellationToken.None));
    }

    [Fact]
    public void A_cli_package_without_a_source_revision_is_not_installable_from_a_release()
    {
        Assert.Equal(new ReleaseIdentity("0.1.0-preview.1", "abc123"), ReleaseIdentity.Parse("0.1.0-preview.1+abc123"));
        var error = Assert.Throws<InvalidOperationException>(() => ReleaseIdentity.Parse("0.1.0-preview.1"));
        Assert.Contains("no source revision", error.Message);
    }
}
