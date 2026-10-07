using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class ReleaseTests
{
    private static readonly HttpClient Client = new(new StaticOriginHandler(Path.GetTempPath()));

    [Fact]
    public async Task The_release_the_cli_was_built_from_is_accepted()
    {
        using var fixture = new ReleaseFixture();
        var source = ReleaseSource.Open(fixture.Release, Client);
        var manifest = await Release.ReadAsync(source, CancellationToken.None);
        Release.RequireThisPlatform(manifest, source);
        Release.RequireSameRelease(manifest, fixture.Cli, source);
        Assert.Equal(ReleaseFixture.PackageIds.Length, manifest.Packages.Length);
    }

    [Fact]
    public async Task A_release_of_another_version_is_refused_with_the_command_that_installs_its_cli()
    {
        using var fixture = new ReleaseFixture();
        fixture.DescribeRelease("0.1.0-preview.2", ReleaseFixture.Revision);
        var source = ReleaseSource.Open(fixture.Release, Client);
        var manifest = await Release.ReadAsync(source, CancellationToken.None);
        var error = Assert.Throws<InvalidOperationException>(() => Release.RequireSameRelease(manifest, fixture.Cli, source));
        Assert.Contains($"dotnet tool update --global Isoplane.Cli --version 0.1.0-preview.2 --add-source {Path.Combine(fixture.Release, "packages")}", error.Message);
    }

    [Fact]
    public async Task A_release_assembled_from_another_revision_of_the_same_version_is_refused()
    {
        using var fixture = new ReleaseFixture();
        fixture.DescribeRelease(ReleaseFixture.Version, "fedcba9876543210fedcba9876543210fedcba98");
        var source = ReleaseSource.Open(fixture.Release, Client);
        var manifest = await Release.ReadAsync(source, CancellationToken.None);
        var error = Assert.Throws<InvalidOperationException>(() => Release.RequireSameRelease(manifest, fixture.Cli, source));
        Assert.Contains("revision fedcba987654", error.Message);
    }

    [Fact]
    public async Task A_release_for_another_platform_is_refused_before_anything_is_fetched()
    {
        using var fixture = new ReleaseFixture();
        var path = Path.Combine(fixture.Release, Release.ManifestName);
        var manifest = JsonSerializer.Deserialize<ReleaseManifest>(await File.ReadAllTextAsync(path), new JsonSerializerOptions { PropertyNameCaseInsensitive = true })!;
        var foreignPlatform = manifest.RuntimeIdentifier == "osx-arm64" ? "linux-x64" : "osx-arm64";
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(manifest with { RuntimeIdentifier = foreignPlatform }));
        var source = ReleaseSource.Open(fixture.Release, Client);
        var foreignManifest = await Release.ReadAsync(source, CancellationToken.None);
        var error = Assert.Throws<InvalidOperationException>(() => Release.RequireThisPlatform(foreignManifest, source));
        Assert.True(error.Message.Contains(foreignPlatform, StringComparison.Ordinal), "A foreign release must be rejected with the platform it targets.");
    }

    [Fact]
    public async Task A_directory_without_a_manifest_is_not_a_release()
    {
        using var fixture = new ReleaseFixture();
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Release.ReadAsync(ReleaseSource.Open(fixture.Source, Client), CancellationToken.None));
        Assert.Contains(Release.ManifestName, error.Message);
    }

    [Fact]
    public async Task A_manifest_missing_the_release_identity_is_refused()
    {
        using var fixture = new ReleaseFixture();
        await File.WriteAllTextAsync(Path.Combine(fixture.Release, Release.ManifestName), JsonSerializer.Serialize(new { packageVersion = ReleaseFixture.Version }));
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Release.ReadAsync(ReleaseSource.Open(fixture.Release, Client), CancellationToken.None));
        Assert.Contains("does not identify", error.Message);
    }

    [Fact]
    public async Task A_manifest_naming_an_asset_outside_the_release_is_refused()
    {
        using var fixture = new ReleaseFixture();
        var path = Path.Combine(fixture.Release, Release.ManifestName);
        var manifest = JsonSerializer.Deserialize<ReleaseManifest>(await File.ReadAllTextAsync(path), new JsonSerializerOptions { PropertyNameCaseInsensitive = true })!;
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(manifest with { Packages = [manifest.Packages[0] with { File = "../outside.nupkg" }] }));
        await Assert.ThrowsAsync<InvalidOperationException>(() => Release.ReadAsync(ReleaseSource.Open(fixture.Release, Client), CancellationToken.None));
    }

    [Fact]
    public void A_cli_package_without_a_source_revision_is_not_installable_from_a_release()
    {
        Assert.Equal(new ReleaseIdentity("0.1.0-preview.1", "abc123"), ReleaseIdentity.Parse("0.1.0-preview.1+abc123"));
        var error = Assert.Throws<InvalidOperationException>(() => ReleaseIdentity.Parse("0.1.0-preview.1"));
        Assert.Contains("no source revision", error.Message);
    }

}
