using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class ReleaseSourceTests
{
    [Fact]
    public async Task An_unpublished_asset_is_reported_with_its_url()
    {
        using var fixture = new ToolchainBundle();
        var handler = new StaticOriginHandler(fixture.Root);
        handler.Missing.Add("release/packages/Isoplane.Sdk.0.1.0-preview.1.nupkg");
        var source = ReleaseSource.Open("http://origin.test/release/", new HttpClient(handler));
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => source.FetchAsync("packages/Isoplane.Sdk.0.1.0-preview.1.nupkg", Path.Combine(fixture.Home, "x.nupkg"), new string('0', 64), null, CancellationToken.None));
        Assert.Contains("http://origin.test/release/packages/Isoplane.Sdk.0.1.0-preview.1.nupkg", error.Message);
        Assert.Contains("404", error.Message);
        Assert.False(File.Exists(Path.Combine(fixture.Home, "x.nupkg")), "a failed download must not leave a file that looks installed");
    }

    [Fact]
    public async Task An_asset_whose_digest_differs_from_the_manifest_is_refused_and_not_kept()
    {
        using var fixture = new ToolchainBundle();
        var source = ReleaseSource.Open(fixture.Release, new HttpClient(new StaticOriginHandler(fixture.Root)));
        var destination = Path.Combine(fixture.Home, "toolchain.tgz");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => source.FetchAsync("toolchain.tgz", destination, new string('a', 64), null, CancellationToken.None));
        Assert.Contains("does not match the release manifest", error.Message);
        Assert.False(File.Exists(destination), "an altered asset must not be kept");
        Assert.False(File.Exists(destination + ".part"), "a refused copy leaves no partial file");
    }

    [Fact]
    public async Task A_download_cut_short_is_reported_as_interrupted_and_retryable()
    {
        using var fixture = new ToolchainBundle();
        var handler = new StaticOriginHandler(fixture.Root);
        handler.Truncated.Add("release/toolchain.tgz");
        var source = ReleaseSource.Open("http://origin.test/release/", new HttpClient(handler));
        var expected = await ReleaseSource.DigestAsync(fixture.ArchivePath, CancellationToken.None);
        var destination = Path.Combine(fixture.Home, "toolchain.tgz");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => source.FetchAsync("toolchain.tgz", destination, expected, null, CancellationToken.None));
        Assert.Contains("interrupted", error.Message);
        handler.Truncated.Clear();
        Assert.True(await source.FetchAsync("toolchain.tgz", destination, expected, null, CancellationToken.None), "the retry must copy the asset");
        Assert.False(await source.FetchAsync("toolchain.tgz", destination, expected, null, CancellationToken.None), "a verified copy is reused rather than downloaded again");
    }

    [Fact]
    public async Task A_verified_local_copy_is_reused_only_when_its_digest_matches()
    {
        using var fixture = new ToolchainBundle();
        var source = ReleaseSource.Open(fixture.Release, new HttpClient(new StaticOriginHandler(fixture.Root)));
        var expected = await ReleaseSource.DigestAsync(fixture.ArchivePath, CancellationToken.None);
        var destination = Path.Combine(fixture.Home, "toolchain.tgz");
        Directory.CreateDirectory(fixture.Home);
        await File.WriteAllTextAsync(destination, "stale");
        Assert.True(await source.FetchAsync("toolchain.tgz", destination, expected, null, CancellationToken.None), "a stale copy must be replaced");
        Assert.Equal(expected, await ReleaseSource.DigestAsync(destination, CancellationToken.None));
    }
}
