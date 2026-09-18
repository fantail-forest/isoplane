using Xunit;

namespace Wisp.Cli.Tests;

public sealed class InstallationTests : IDisposable
{
    private readonly string home = Directory.CreateTempSubdirectory("isoplane-installation-").FullName;
    private static readonly ReleaseIdentity Cli = new("0.1.0-preview.1", "0123456789abcdef0123456789abcdef01234567");

    public void Dispose() => Directory.Delete(home, recursive: true);

    [Fact]
    public void Nothing_is_recorded_until_setup_begins()
    {
        Assert.Null(Installation.Load(home));
    }

    [Fact]
    public void Each_record_is_persisted_as_it_is_made()
    {
        var installation = Installation.Begin(home, Cli);
        installation.Record(new(InstalledResourceKind.Directory, "/tmp/a", Owned: true));
        installation.Record(new(InstalledResourceKind.PackageSource, "isoplane", Owned: false, Detail: "/tmp/feed"));
        var reloaded = Installation.Load(home)!;
        Assert.Equal(2, reloaded.Resources.Count);
        Assert.Equal(new InstalledResource(InstalledResourceKind.PackageSource, "isoplane", false, "/tmp/feed"), reloaded.Find(InstalledResourceKind.PackageSource, "isoplane"));
    }

    [Fact]
    public void Recording_the_same_resource_again_replaces_its_record()
    {
        var installation = Installation.Begin(home, Cli);
        installation.Record(new(InstalledResourceKind.Template, "Wisp.Templates", Owned: true, Detail: "1"));
        installation.Record(new(InstalledResourceKind.Template, "Wisp.Templates", Owned: true, Detail: "2"));
        Assert.Equal("2", Assert.Single(Installation.Load(home)!.Resources).Detail);
    }

    [Fact]
    public void Beginning_again_keeps_the_records_and_stamps_the_installing_cli()
    {
        Installation.Begin(home, Cli).Record(new(InstalledResourceKind.File, "/tmp/f", Owned: true));
        var next = Installation.Begin(home, new ReleaseIdentity("0.1.0-preview.2", "fedcba9876543210fedcba9876543210fedcba98"));
        Assert.Single(next.Resources);
        Assert.Equal("0.1.0-preview.2", Installation.Load(home)!.CliVersion);
    }

    [Fact]
    public void A_manifest_from_another_schema_is_refused_rather_than_guessed_at()
    {
        File.WriteAllText(Installation.PathIn(home), "{\"schema\":99,\"cliVersion\":\"1\",\"sourceRevision\":\"a\",\"home\":\"/h\",\"resources\":[]}");
        Assert.Throws<InstallationManifestException>(() => Installation.Load(home));
    }
}
