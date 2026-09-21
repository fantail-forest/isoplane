using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class SdkRemovalTests
{
    private static async Task<FakeRegistrations> SetUpAsync(ToolchainBundle fixture, FakeRegistrations? registrations = null)
    {
        registrations ??= new FakeRegistrations();
        Assert.Equal(0, await Setup.RunAsync(new SetupOptions(fixture.Release, false), fixture.Cli, fixture.Home, Machines.Ready(fixture, registrations), new HttpClient(new StaticOriginHandler(fixture.Root)), CancellationToken.None));
        return registrations;
    }

    private static Task<int> RemoveAsync(ToolchainBundle fixture, FakeRegistrations registrations) =>
        SdkRemoval.RunAsync(fixture.Home, _ => registrations, CancellationToken.None, new FakeWorkloadInstallation(registrations));

    [Fact]
    public async Task A_failed_template_removal_keeps_the_windows_installer_available_for_retry()
    {
        using var fixture = new ToolchainBundle();
        Directory.CreateDirectory(fixture.Home);
        var installation = Installation.Begin(fixture.Home, fixture.Cli);
        installation.Record(new(InstalledResourceKind.WindowsInstaller, "product", true));
        installation.Record(new(InstalledResourceKind.Template, "Isoplane.Templates", true));
        var registrations = new FakeRegistrations { FailUninstallTemplate = _ => new InvalidOperationException("template busy") };
        registrations.Templates["Isoplane.Templates"] = ToolchainBundle.Version;
        var installer = new RecordingInstaller();

        var failed = await SdkRemoval.RunAsync(fixture.Home, _ => registrations, CancellationToken.None, installer);
        Assert.True(failed == 1, "template removal failure must make SDK removal fail");
        Assert.True(installer.Removed.Count == 0, "the MSI must remain installed while its template registration remains");
        Assert.True(Installation.Load(fixture.Home)!.Resources.Count == 2, "both failed resources must remain recorded for retry");

        registrations.FailUninstallTemplate = null;
        var retried = await SdkRemoval.RunAsync(fixture.Home, _ => registrations, CancellationToken.None, installer);
        Assert.True(retried == 0, "removal must succeed after the template engine recovers");
        Assert.True(installer.Removed.Count == 1, "the retry must remove the retained MSI");
    }

    private sealed class RecordingInstaller : ISdkInstallation
    {
        internal List<InstalledResource> Removed { get; } = [];
        public Task<string> InstallAsync(string bundle, string version, Installation installation, CancellationToken cancellation) =>
            throw new NotSupportedException();
        public Task RemoveAsync(InstalledResource resource, CancellationToken cancellation)
        {
            Removed.Add(resource);
            return Task.CompletedTask;
        }
    }

    [Fact]
    public async Task Removal_takes_away_everything_setup_recorded_and_nothing_else()
    {
        using var fixture = new ToolchainBundle();
        var registrations = await SetUpAsync(fixture);
        var config = Path.Combine(fixture.Home, "config.json");
        await File.WriteAllTextAsync(config, "{}");
        await File.AppendAllTextAsync(Path.Combine(fixture.UserHome, ".profile"), "export MINE=1\n");

        Assert.Equal(0, await RemoveAsync(fixture, registrations));

        Assert.False(File.Exists(Installation.PathIn(fixture.Home)), "a complete removal deletes the manifest");
        Assert.False(Directory.Exists(Toolchain.Installations(fixture.Home)), "toolchains are removed");
        Assert.False(Directory.Exists(Path.Combine(fixture.Home, "packages")), "the package feed is removed");
        Assert.False(File.Exists(Path.Combine(fixture.Home, "current.json")), "the toolchain selection is removed");
        Assert.False(File.Exists(Path.Combine(fixture.Home, "toolchain.props")), "the MSBuild selection is removed");
        Assert.Null(new DirectoryInfo(Path.Combine(fixture.Home, "dotnet")).LinkTarget);
        Assert.False(File.Exists(Path.Combine(fixture.Home, "env")), "the environment file is removed");
        Assert.Empty(registrations.Sources);
        Assert.Empty(registrations.Templates);
        Assert.Equal("# user profile\nexport MINE=1\n", await File.ReadAllTextAsync(Path.Combine(fixture.UserHome, ".profile")));
        Assert.True(File.Exists(config), "CLI configuration is not part of the SDK");
    }

    [Fact]
    public async Task Setup_installs_a_working_sdk_again_after_removal()
    {
        using var fixture = new ToolchainBundle();
        var registrations = await SetUpAsync(fixture);
        Assert.Equal(0, await RemoveAsync(fixture, registrations));
        await SetUpAsync(fixture, registrations);
        Assert.NotNull(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
        Assert.Equal(ToolchainBundle.Version, registrations.Templates["Isoplane.Templates"]);
    }

    [Fact]
    public async Task Pre_existing_resources_are_kept_and_reported()
    {
        using var fixture = new ToolchainBundle();
        var registrations = new FakeRegistrations();
        registrations.Sources["isoplane"] = Path.Combine(fixture.Home, "packages");
        await SetUpAsync(fixture, registrations);
        Assert.Equal(0, await RemoveAsync(fixture, registrations));
        Assert.Equal(Path.Combine(fixture.Home, "packages"), registrations.Sources["isoplane"]);
        Assert.Empty(registrations.Templates);
    }

    [Fact]
    public async Task Reused_package_files_survive_setup_and_removal()
    {
        using var fixture = new ToolchainBundle();
        var packages = Path.Combine(fixture.Home, "packages");
        Directory.CreateDirectory(packages);
        var name = $"Isoplane.Sdk.{ToolchainBundle.Version}.nupkg";
        var existing = Path.Combine(packages, name);
        File.Copy(Path.Combine(fixture.Release, "packages", name), existing);
        var original = File.ReadAllBytes(existing);
        var registrations = await SetUpAsync(fixture);
        Assert.True(await RemoveAsync(fixture, registrations) == 0, "removal must complete with reused packages present");
        Assert.True(File.ReadAllBytes(existing).SequenceEqual(original), "setup must not claim ownership of an existing package");
    }

    [Fact]
    public async Task A_missing_manifest_is_a_diagnostic_and_removes_nothing()
    {
        using var fixture = new ToolchainBundle();
        await fixture.InstallAsync();
        Assert.Equal(1, await RemoveAsync(fixture, new FakeRegistrations()));
        Assert.NotNull(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
    }

    [Fact]
    public async Task A_damaged_manifest_is_a_diagnostic_and_removes_nothing()
    {
        using var fixture = new ToolchainBundle();
        var registrations = await SetUpAsync(fixture);
        await File.WriteAllTextAsync(Installation.PathIn(fixture.Home), "{ not json");
        var error = await Assert.ThrowsAsync<InstallationManifestException>(() => RemoveAsync(fixture, registrations));
        Assert.Contains(Installation.PathIn(fixture.Home), error.Message);
        Assert.NotNull(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
        Assert.Single(registrations.Templates);
    }

    [Fact]
    public async Task A_failed_removal_keeps_the_entry_for_retry_and_does_not_claim_success()
    {
        using var fixture = new ToolchainBundle();
        var registrations = await SetUpAsync(fixture);
        registrations.FailUninstallTemplate = _ => new InvalidOperationException("template engine busy");
        Assert.Equal(1, await RemoveAsync(fixture, registrations));
        var manifest = JsonDocument.Parse(await File.ReadAllTextAsync(Installation.PathIn(fixture.Home)));
        var remaining = manifest.RootElement.GetProperty("resources").EnumerateArray().ToList();
        var template = Assert.Single(remaining, resource => resource.GetProperty("kind").GetString() == "Workload");
        Assert.Equal("template engine busy", template.GetProperty("removalFailure").GetString());
        Assert.Single(registrations.Templates);

        registrations.FailUninstallTemplate = null;
        Assert.Equal(0, await RemoveAsync(fixture, registrations));
        Assert.False(File.Exists(Installation.PathIn(fixture.Home)), "the retry completes the removal");
        Assert.Empty(registrations.Templates);
    }

    [Fact]
    public async Task Removal_after_an_interrupted_setup_cleans_up_the_partial_installation()
    {
        using var fixture = new ToolchainBundle();
        var handler = new StaticOriginHandler(fixture.Root);
        handler.Truncated.Add("release/toolchain.tgz");
        var registrations = new FakeRegistrations();
        await Assert.ThrowsAsync<InvalidOperationException>(() => Setup.RunAsync(new SetupOptions("http://origin.test/release/", false), fixture.Cli, fixture.Home, Machines.Ready(fixture, registrations), new HttpClient(handler), CancellationToken.None));
        Assert.True(Directory.Exists(Path.Combine(fixture.Home, "packages")), "the interrupted setup left packages behind");
        Assert.Equal(0, await RemoveAsync(fixture, registrations));
        Assert.False(Directory.Exists(Path.Combine(fixture.Home, "packages")), "removal cleans up the partial installation");
        Assert.False(Directory.Exists(Path.Combine(fixture.Home, "downloads")), "removal cleans up partial downloads");
    }

    [Fact]
    public async Task Removal_after_an_upgrade_removes_the_replacement_toolchain_and_its_dangling_selection_link()
    {
        using var fixture = new ToolchainBundle();
        var registrations = await SetUpAsync(fixture);
        await File.WriteAllTextAsync(Path.Combine(fixture.Source, "bin", "tool"), "next payload");
        var manifestPath = Path.Combine(fixture.Source, "toolchain.json");
        var manifest = JsonSerializer.Deserialize<ToolchainManifest>(await File.ReadAllTextAsync(manifestPath))!;
        await File.WriteAllTextAsync(manifestPath, JsonSerializer.Serialize(manifest with
        {
            Files = manifest.Files.Select(file => file.Path == "bin/tool" ? file with { Sha256 = Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData("next payload"u8)) } : file).ToArray()
        }));
        var next = new ReleaseIdentity("0.1.0-preview.2", "fedcba9876543210fedcba9876543210fedcba98");
        fixture.DescribeRelease(next.Version, next.SourceRevision);
        Assert.Equal(0, await Setup.RunAsync(new SetupOptions(fixture.Release, false), next, fixture.Home, Machines.Ready(fixture, registrations), new HttpClient(new StaticOriginHandler(fixture.Root)), CancellationToken.None));

        Assert.Equal(0, await RemoveAsync(fixture, registrations));
        Assert.False(Directory.Exists(Toolchain.Installations(fixture.Home)), "the replacement toolchain is removed");
        Assert.Null(new FileInfo(Path.Combine(fixture.Home, "dotnet")).LinkTarget);
        Assert.False(File.Exists(Installation.PathIn(fixture.Home)), "a complete removal deletes the manifest");
        Assert.Empty(registrations.Templates);
    }

    [Fact]
    public async Task Removing_twice_is_safe()
    {
        using var fixture = new ToolchainBundle();
        var registrations = await SetUpAsync(fixture);
        Assert.Equal(0, await RemoveAsync(fixture, registrations));
        Assert.Equal(1, await RemoveAsync(fixture, registrations));
    }
}
