using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class SetupTests
{
    private static HttpClient Client(ToolchainBundle fixture) => new(new StaticOriginHandler(fixture.Root));

    private static Task<int> RunAsync(ToolchainBundle fixture, SetupMachine machine, string? source = null, bool yes = false, ReleaseIdentity? cli = null, HttpClient? client = null) =>
        Setup.RunAsync(new SetupOptions(source ?? fixture.Release, yes), cli ?? fixture.Cli, fixture.Home, machine, client ?? Client(fixture), CancellationToken.None);

    private static JsonDocument Manifest(ToolchainBundle fixture) => JsonDocument.Parse(File.ReadAllText(Installation.PathIn(fixture.Home)));

    [Fact]
    public async Task A_clean_setup_installs_every_component_and_records_each_in_the_installation_manifest()
    {
        using var fixture = new ToolchainBundle();
        var registrations = new FakeRegistrations();
        Assert.Equal(0, await RunAsync(fixture, Machines.Ready(fixture, registrations)));

        var toolchain = await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None);
        Assert.Equal(Path.Combine(Toolchain.Installations(fixture.Home), fixture.Identity), toolchain);
        foreach (var id in ToolchainBundle.PackageIds)
            Assert.True(File.Exists(Path.Combine(fixture.Home, "packages", $"{id}.{ToolchainBundle.Version}.nupkg")), $"the {id} package must be installed into the feed");
        Assert.Equal(Path.Combine(fixture.Home, "packages"), registrations.Sources["isoplane"]);
        Assert.Equal(ToolchainBundle.Version, registrations.Templates["Isoplane.Templates"]);
        Assert.False(Directory.Exists(Path.Combine(fixture.Home, "downloads")), "the toolchain archive is not kept after installation");
        var profile = await File.ReadAllTextAsync(Path.Combine(fixture.UserHome, ".profile"));
        Assert.Equal("# user profile\n", profile);
        Assert.StartsWith("# user profile", profile);
        Assert.False(File.Exists(Path.Combine(fixture.Home, "env")), "setup must not change shell SDK selection");

        var manifest = Manifest(fixture);
        var recorded = manifest.RootElement.GetProperty("resources").EnumerateArray()
            .Select(resource => (Kind: resource.GetProperty("kind").GetString(), Target: resource.GetProperty("target").GetString(), Owned: resource.GetProperty("owned").GetBoolean()))
            .ToList();
        Assert.Contains(("Directory", toolchain, true), recorded);
        Assert.Contains(("PackageSource", "isoplane", true), recorded);
        Assert.Contains(("Workload", "isoplane", true), recorded);
        Assert.DoesNotContain(recorded, resource => resource.Target!.Contains("downloads", StringComparison.Ordinal));
        Assert.Equal(ToolchainBundle.Version, manifest.RootElement.GetProperty("cliVersion").GetString());
    }

    [Fact]
    public async Task Running_setup_again_changes_nothing()
    {
        using var fixture = new ToolchainBundle();
        var registrations = new FakeRegistrations();
        Assert.Equal(0, await RunAsync(fixture, Machines.Ready(fixture, registrations)));
        var manifest = await File.ReadAllTextAsync(Installation.PathIn(fixture.Home));
        var profile = await File.ReadAllTextAsync(Path.Combine(fixture.UserHome, ".profile"));
        var calls = registrations.Calls.Count;
        Assert.Equal(0, await RunAsync(fixture, Machines.Ready(fixture, registrations)));
        Assert.Equal(manifest, await File.ReadAllTextAsync(Installation.PathIn(fixture.Home)));
        Assert.Equal(profile, await File.ReadAllTextAsync(Path.Combine(fixture.UserHome, ".profile")));
        Assert.Equal(calls, registrations.Calls.Count);
    }

    [Fact]
    public async Task Setup_downloads_the_release_over_http_and_installs_it()
    {
        using var fixture = new ToolchainBundle();
        var handler = new StaticOriginHandler(fixture.Root);
        var registrations = new FakeRegistrations();
        Assert.Equal(0, await RunAsync(fixture, Machines.Ready(fixture, registrations), "http://origin.test/release/", client: new HttpClient(handler)));
        Assert.Contains("release/toolchain.tgz", handler.Requests);
        Assert.Contains("release/packages/Isoplane.Templates.0.1.0-preview.1.nupkg", handler.Requests);
        Assert.NotNull(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
    }

    [Fact]
    public async Task An_interrupted_toolchain_download_leaves_no_toolchain_selected_and_a_manifest_describing_the_partial_install()
    {
        using var fixture = new ToolchainBundle();
        var handler = new StaticOriginHandler(fixture.Root);
        handler.Truncated.Add("release/toolchain.tgz");
        var registrations = new FakeRegistrations();
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, Machines.Ready(fixture, registrations), "http://origin.test/release/", client: new HttpClient(handler)));
        Assert.Contains("interrupted", error.Message);
        Assert.False(File.Exists(Path.Combine(fixture.Home, "current.json")), "an incomplete download must not select a toolchain");
        var recorded = Manifest(fixture).RootElement.GetProperty("resources").EnumerateArray().Select(resource => resource.GetProperty("target").GetString()!).ToList();
        Assert.Contains(Path.Combine(fixture.Home, "downloads"), recorded);
        Assert.Contains(Path.Combine(fixture.Home, "packages"), recorded);
        Assert.Empty(registrations.Templates);

        handler.Truncated.Clear();
        Assert.Equal(0, await RunAsync(fixture, Machines.Ready(fixture, registrations), "http://origin.test/release/", client: new HttpClient(handler)));
        Assert.NotNull(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
    }

    [Fact]
    public async Task A_release_missing_from_the_origin_is_reported_with_its_url()
    {
        using var fixture = new ToolchainBundle();
        var handler = new StaticOriginHandler(fixture.Root);
        handler.Missing.Add("release/release.json");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, Machines.Ready(fixture, new FakeRegistrations()), "http://origin.test/release/", client: new HttpClient(handler)));
        Assert.Contains("http://origin.test/release/release.json", error.Message);
        Assert.False(Directory.Exists(fixture.Home), "nothing is installed when the release cannot be read");
    }

    [Fact]
    public async Task Missing_prerequisites_stop_setup_before_any_download_and_name_the_exact_command()
    {
        using var fixture = new ToolchainBundle();
        var asked = new List<string>();
        var machine = Machines.Ready(fixture, new FakeRegistrations()) with
        {
            CanLoad = soname => soname != "libzstd.so.1" && soname != "libtinfo.so.6",
            Confirm = question => { asked.Add(question); return Task.FromResult(false); },
        };
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, machine));
        Assert.Contains("Setup is incomplete", error.Message);
        Assert.Contains("sudo apt-get install -y libzstd1 libtinfo6", error.Message);
        Assert.Single(asked);
        Assert.False(Directory.Exists(fixture.Home), "nothing is downloaded while prerequisites are missing");
    }

    [Fact]
    public async Task Missing_prerequisites_are_installed_with_the_privileged_command_once_confirmed()
    {
        using var fixture = new ToolchainBundle();
        var installed = false;
        var commands = new List<string>();
        var machine = Machines.Ready(fixture, new FakeRegistrations()) with
        {
            CanLoad = soname => installed || soname != "libssl.so.3",
            Confirm = _ => Task.FromResult(true),
            RunPrivileged = (command, _) => { commands.Add(command); installed = true; return Task.FromResult(0); },
        };
        Assert.Equal(0, await RunAsync(fixture, machine));
        Assert.Equal(["sudo apt-get install -y libssl3t64"], commands);
    }

    [Fact]
    public async Task Yes_installs_missing_prerequisites_without_a_prompt()
    {
        using var fixture = new ToolchainBundle();
        var installed = false;
        var machine = Machines.Ready(fixture, new FakeRegistrations()) with
        {
            CanLoad = soname => installed || !soname.StartsWith("libicuuc", StringComparison.Ordinal),
            RunPrivileged = (_, _) => { installed = true; return Task.FromResult(0); },
        };
        Assert.Equal(0, await RunAsync(fixture, machine, yes: true));
    }

    [Fact]
    public async Task A_privileged_install_that_leaves_prerequisites_missing_is_an_incomplete_setup()
    {
        using var fixture = new ToolchainBundle();
        var machine = Machines.Ready(fixture, new FakeRegistrations()) with
        {
            FileExists = path => path != Prerequisites.CertificateBundle,
            RunPrivileged = (_, _) => Task.FromResult(100),
        };
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, machine, yes: true));
        Assert.Contains("exited 100", error.Message);
        Assert.Contains("CA certificates", error.Message);
        Assert.False(Directory.Exists(fixture.Home), "an incomplete setup must not install anything");
    }

    [Fact]
    public async Task An_unsupported_distribution_is_told_which_libraries_it_lacks()
    {
        using var fixture = new ToolchainBundle();
        var machine = Machines.Ready(fixture, new FakeRegistrations()) with
        {
            CanLoad = soname => soname != "libtinfo.so.6",
            OperatingSystem = new OperatingSystemRelease("fedora", "41"),
        };
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, machine));
        Assert.Contains("libtinfo.so.6", error.Message);
        Assert.DoesNotContain("sudo apt-get", error.Message);
    }

    [Fact]
    public async Task A_release_of_another_version_is_refused_before_installing_anything()
    {
        using var fixture = new ToolchainBundle();
        fixture.DescribeRelease("0.1.0-preview.2", ToolchainBundle.Revision);
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, Machines.Ready(fixture, new FakeRegistrations())));
        Assert.Contains("dotnet tool update --global Isoplane.Cli --version 0.1.0-preview.2", error.Message);
        Assert.False(Directory.Exists(fixture.Home), "a toolchain from another release must not be installed beside this CLI");
    }

    [Fact]
    public async Task An_altered_toolchain_archive_is_refused_and_leaves_no_toolchain()
    {
        using var fixture = new ToolchainBundle();
        await File.WriteAllTextAsync(Path.Combine(fixture.Source, "toolchain.json"), await File.ReadAllTextAsync(Path.Combine(fixture.Source, "toolchain.json")) + "\n");
        fixture.Archive();
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, Machines.Ready(fixture, new FakeRegistrations())));
        Assert.Contains("does not match the release manifest", error.Message);
        Assert.False(File.Exists(Path.Combine(fixture.Home, "current.json")), "an altered toolchain must not be selected");
        Assert.False(Directory.Exists(Toolchain.Installations(fixture.Home)), "an altered archive is refused before extraction");
    }

    [Fact]
    public async Task A_toolchain_archive_other_than_the_one_the_manifest_identifies_is_refused_after_extraction()
    {
        using var fixture = new ToolchainBundle();
        var manifest = fixture.DescribeRelease(ToolchainBundle.Version, ToolchainBundle.Revision);
        await File.WriteAllTextAsync(Path.Combine(fixture.Release, Release.ManifestName),
            JsonSerializer.Serialize(manifest with { Toolchain = manifest.Toolchain with { ManifestSha256 = new string('b', 64) } }));
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, Machines.Ready(fixture, new FakeRegistrations())));
        Assert.Contains("not the one the release manifest describes", error.Message);
        Assert.False(File.Exists(Path.Combine(fixture.Home, "current.json")), "a toolchain the release does not describe must not be selected");
        Assert.Empty(Directory.GetDirectories(Toolchain.Installations(fixture.Home)));
    }

    [Fact]
    public async Task A_package_source_of_the_same_name_pointing_elsewhere_is_refused()
    {
        using var fixture = new ToolchainBundle();
        var registrations = new FakeRegistrations();
        registrations.Sources["isoplane"] = "/srv/other-feed";
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => RunAsync(fixture, Machines.Ready(fixture, registrations)));
        Assert.Contains("/srv/other-feed", error.Message);
        Assert.Equal("/srv/other-feed", registrations.Sources["isoplane"]);
    }

    [Fact]
    public async Task A_package_source_already_pointing_at_the_feed_is_reused_and_recorded_as_not_owned()
    {
        using var fixture = new ToolchainBundle();
        var registrations = new FakeRegistrations();
        registrations.Sources["isoplane"] = Path.Combine(fixture.Home, "packages");
        Assert.Equal(0, await RunAsync(fixture, Machines.Ready(fixture, registrations)));
        Assert.DoesNotContain("add source isoplane", registrations.Calls);
        var source = Manifest(fixture).RootElement.GetProperty("resources").EnumerateArray().Single(resource => resource.GetProperty("kind").GetString() == "PackageSource");
        Assert.False(source.GetProperty("owned").GetBoolean(), "a pre-existing source is not owned by setup");
    }

    [Fact]
    public async Task Upgrading_replaces_the_toolchain_and_template_and_keeps_earlier_packages_for_existing_projects()
    {
        using var fixture = new ToolchainBundle();
        var registrations = new FakeRegistrations();
        Assert.Equal(0, await RunAsync(fixture, Machines.Ready(fixture, registrations)));
        var first = await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None);

        await File.WriteAllTextAsync(Path.Combine(fixture.Source, "bin", "tool"), "next payload");
        var manifestPath = Path.Combine(fixture.Source, "toolchain.json");
        var manifest = JsonSerializer.Deserialize<ToolchainManifest>(await File.ReadAllTextAsync(manifestPath))!;
        await File.WriteAllTextAsync(manifestPath, JsonSerializer.Serialize(manifest with
        {
            Files = manifest.Files.Select(file => file.Path == "bin/tool" ? file with { Sha256 = Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData("next payload"u8)) } : file).ToArray()
        }));
        var next = new ReleaseIdentity("0.1.0-preview.2", "fedcba9876543210fedcba9876543210fedcba98");
        fixture.DescribeRelease(next.Version, next.SourceRevision);
        Assert.Equal(0, await RunAsync(fixture, Machines.Ready(fixture, registrations), cli: next));

        var second = await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None);
        Assert.NotEqual(first, second);
        Assert.False(Directory.Exists(first), "the superseded toolchain is removed");
        Assert.Equal("0.1.0-preview.2", registrations.Templates["Isoplane.Templates"]);
        Assert.True(File.Exists(Path.Combine(fixture.Home, "packages", "Isoplane.Sdk.0.1.0-preview.1.nupkg")), "projects pinned to the earlier SDK still restore");
        Assert.True(File.Exists(Path.Combine(fixture.Home, "packages", "Isoplane.Sdk.0.1.0-preview.2.nupkg")), "the new SDK is installed");
        Assert.Equal("0.1.0-preview.2", Manifest(fixture).RootElement.GetProperty("cliVersion").GetString());
    }

    [Fact]
    public void Setup_options_take_a_source_and_consent()
    {
        Assert.Equal(new SetupOptions(null, false), SetupOptions.Parse(["setup"]));
        Assert.Equal(new SetupOptions("/tmp/release", true), SetupOptions.Parse(["setup", "--source", "/tmp/release", "--yes"]));
    }
}
