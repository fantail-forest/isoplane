using System.Text.Json;
using Xunit;

namespace Wisp.Cli.Tests;

public sealed class ReleaseTests
{
    private static readonly HttpClient Client = new(new StaticOriginHandler(Path.GetTempPath()));

    [Fact]
    public async Task The_release_the_cli_was_built_from_is_accepted()
    {
        using var fixture = new ToolchainBundle();
        var source = ReleaseSource.Open(fixture.Release, Client);
        var manifest = await Release.ReadAsync(source, CancellationToken.None);
        Release.RequireThisPlatform(manifest, source);
        Release.RequireSameRelease(manifest, fixture.Cli, source);
        Assert.Equal(fixture.Identity, manifest.Toolchain.ManifestSha256);
    }

    [Fact]
    public async Task A_release_of_another_version_is_refused_with_the_command_that_installs_its_cli()
    {
        using var fixture = new ToolchainBundle();
        fixture.DescribeRelease("0.1.0-preview.2", ToolchainBundle.Revision);
        var source = ReleaseSource.Open(fixture.Release, Client);
        var manifest = await Release.ReadAsync(source, CancellationToken.None);
        var error = Assert.Throws<InvalidOperationException>(() => Release.RequireSameRelease(manifest, fixture.Cli, source));
        Assert.Contains($"dotnet tool update --global Isoplane.Cli --version 0.1.0-preview.2 --add-source {Path.Combine(fixture.Release, "packages")}", error.Message);
    }

    [Fact]
    public async Task A_release_assembled_from_another_revision_of_the_same_version_is_refused()
    {
        using var fixture = new ToolchainBundle();
        fixture.DescribeRelease(ToolchainBundle.Version, "fedcba9876543210fedcba9876543210fedcba98");
        var source = ReleaseSource.Open(fixture.Release, Client);
        var manifest = await Release.ReadAsync(source, CancellationToken.None);
        var error = Assert.Throws<InvalidOperationException>(() => Release.RequireSameRelease(manifest, fixture.Cli, source));
        Assert.Contains("revision fedcba987654", error.Message);
    }

    [Fact]
    public async Task A_release_for_another_platform_is_refused_before_anything_is_fetched()
    {
        using var fixture = new ToolchainBundle();
        var path = Path.Combine(fixture.Release, Release.ManifestName);
        var manifest = JsonSerializer.Deserialize<ReleaseManifest>(await File.ReadAllTextAsync(path), new JsonSerializerOptions { PropertyNameCaseInsensitive = true })!;
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(manifest with { RuntimeIdentifier = "osx-arm64" }));
        var source = ReleaseSource.Open(fixture.Release, Client);
        var error = Assert.Throws<InvalidOperationException>(() => Release.RequireThisPlatform(Release.ReadAsync(source, CancellationToken.None).Result, source));
        Assert.Contains("osx-arm64", error.Message);
    }

    [Fact]
    public async Task A_toolchain_directory_alone_is_not_a_release()
    {
        using var fixture = new ToolchainBundle();
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Release.ReadAsync(ReleaseSource.Open(fixture.Source, Client), CancellationToken.None));
        Assert.Contains(Release.ManifestName, error.Message);
    }

    [Fact]
    public async Task A_manifest_missing_the_release_identity_is_refused()
    {
        using var fixture = new ToolchainBundle();
        await File.WriteAllTextAsync(Path.Combine(fixture.Release, Release.ManifestName), JsonSerializer.Serialize(new { packageVersion = ToolchainBundle.Version }));
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Release.ReadAsync(ReleaseSource.Open(fixture.Release, Client), CancellationToken.None));
        Assert.Contains("does not identify", error.Message);
    }

    [Fact]
    public async Task A_manifest_naming_an_asset_outside_the_release_is_refused()
    {
        using var fixture = new ToolchainBundle();
        var path = Path.Combine(fixture.Release, Release.ManifestName);
        var manifest = JsonSerializer.Deserialize<ReleaseManifest>(await File.ReadAllTextAsync(path), new JsonSerializerOptions { PropertyNameCaseInsensitive = true })!;
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(manifest with { Toolchain = manifest.Toolchain with { Archive = "../toolchain.tgz" } }));
        await Assert.ThrowsAsync<InvalidOperationException>(() => Release.ReadAsync(ReleaseSource.Open(fixture.Release, Client), CancellationToken.None));
    }

    [Fact]
    public void A_cli_package_without_a_source_revision_is_not_installable_from_a_release()
    {
        Assert.Equal(new ReleaseIdentity("0.1.0-preview.1", "abc123"), ReleaseIdentity.Parse("0.1.0-preview.1+abc123"));
        var error = Assert.Throws<InvalidOperationException>(() => ReleaseIdentity.Parse("0.1.0-preview.1"));
        Assert.Contains("no source revision", error.Message);
    }

    [Fact]
    public void The_release_at_an_origin_is_the_directory_named_for_the_cli_version()
    {
        var cli = new ReleaseIdentity("0.1.0-preview.1", "abc123");
        var source = ReleaseSource.AtOrigin("https://releases.example.test", cli.ReleaseName, Client);
        Assert.Equal($"https://releases.example.test/isoplane-{Toolchain.PlatformIdentifier()}-0.1.0-preview.1/", source.Location);
        Assert.Equal($"https://releases.example.test/isoplane-{Toolchain.PlatformIdentifier()}-0.1.0-preview.1/packages/a.nupkg", source.Describe("packages/a.nupkg"));
    }

    [Fact]
    public void An_origin_that_is_not_an_http_url_is_refused()
    {
        var error = Assert.Throws<InvalidOperationException>(() => ReleaseSource.AtOrigin("/srv/releases", "isoplane-linux-x64-1", Client));
        Assert.Contains(Release.OriginVariable, error.Message);
    }
}
