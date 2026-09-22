using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class EnvironmentDoctorTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void Backend_checks_accept_a_complete_installation_without_writing_files(bool windows)
    {
        using var fixture = new Backend(windows);
        var before = fixture.Files();
        fixture.Check();
        Assert.True(before.SequenceEqual(fixture.Files()), "Checking registration must not modify the SDK installation.");
    }

    [Theory]
    [InlineData("marker")]
    [InlineData("manifest")]
    [InlineData("targets")]
    [InlineData("props")]
    [InlineData("dependency")]
    [InlineData("band")]
    [InlineData("selection")]
    public void Incomplete_workload_registration_requires_setup_for_the_selected_sdk(string fault)
    {
        using var fixture = new Backend(false);
        switch (fault)
        {
            case "marker": File.Delete(fixture.Marker); break;
            case "manifest": File.Delete(fixture.Manifest); break;
            case "targets": File.Delete(Path.ChangeExtension(fixture.Manifest, "targets")); break;
            case "props": File.Delete(fixture.Props); break;
            case "dependency": Directory.Delete(Path.Combine(fixture.Sdk.Root, "packs", "Test.Dependency"), true); break;
            case "band": fixture.Sdk = fixture.Sdk with { Version = "10.0.500", FeatureBand = "10.0.500" }; break;
            case "selection": fixture.Installed = fixture.Root; break;
        }
        var error = Assert.Throws<InvalidOperationException>(fixture.Check);
        Assert.True((fault == "dependency" || error.Message.Contains(fixture.Sdk.FeatureBand, StringComparison.Ordinal))
            && error.Message.Contains("isoplane setup", StringComparison.Ordinal),
            "The remedy must name the selected feature band and setup command.");
    }

    [Theory]
    [InlineData("{")]
    [InlineData("[]")]
    [InlineData("{\"version\":42}")]
    [InlineData("{\"version\":\"different\"}")]
    public void A_damaged_or_mismatched_workload_manifest_gets_a_repair_command(string contents)
    {
        using var fixture = new Backend(false);
        File.WriteAllText(fixture.Manifest, contents);
        var error = Assert.Throws<InvalidOperationException>(fixture.Check);
        Assert.True(error.Message.Contains("isoplane setup", StringComparison.Ordinal), "Manifest damage must have an actionable repair remedy.");
        Assert.True(File.ReadAllText(fixture.Manifest) == contents, "Doctor must not rewrite damaged registration.");
    }

    [Theory]
    [InlineData("{}")]
    [InlineData("{\"Example\":null}")]
    [InlineData("{\"Example\":{\"kind\":\"unknown\"}}")]
    [InlineData("{\"Example\":{\"kind\":\"sdk\",\"alias-to\":{}}}")]
    [InlineData("{\"Example\":{\"kind\":\"sdk\"}}")]
    public void Invalid_pack_definitions_require_repair(string packs)
    {
        using var fixture = new Backend(false);
        File.WriteAllText(fixture.Manifest, "{\"version\":\"0.1.0-preview.1\",\"packs\":" + packs + "}");
        var error = Assert.Throws<InvalidOperationException>(fixture.Check);
        Assert.True(error.Message.Contains("isoplane setup", StringComparison.Ordinal), "An unusable pack definition must not report readiness.");
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void Missing_windows_registration_or_props_gets_an_msi_repair_remedy(bool missingProps)
    {
        using var fixture = new Backend(true);
        if (missingProps) File.Delete(fixture.Props);
        else fixture.Installation.Forget(fixture.Installation.Resources.Single());
        var error = Assert.Throws<InvalidOperationException>(fixture.Check);
        Assert.True(error.Message.Contains("MSI", StringComparison.Ordinal)
            && error.Message.Contains("isoplane setup", StringComparison.Ordinal)
            && !error.Message.Contains("workload", StringComparison.OrdinalIgnoreCase),
            "Windows must repair its direct MSI installation without requesting a workload.");
    }

    private sealed class Backend : IDisposable
    {
        private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("isoplane-backend-doctor-");
        private readonly bool windows;
        internal string Root => directory.FullName;
        internal Installation Installation { get; }
        internal WorkloadSdk Sdk { get; set; }
        internal string Installed { get; set; }
        internal string Props { get; }
        internal string Manifest { get; }
        internal string Marker { get; }

        internal Backend(bool windows)
        {
            this.windows = windows;
            const string version = "0.1.0-preview.1";
            Sdk = new("dotnet", "10.0.400", "10.0.400", Path.Combine(Root, "dotnet"));
            Installation = Installation.Begin(Path.Combine(Root, "home"), new(version, "test"));
            Installed = windows ? Path.Combine(Root, "msi")
                : Path.Combine(Sdk.Root, "packs", "Isoplane.Toolchain." + Toolchain.PlatformIdentifier(), version);
            Props = Path.Combine(Installed, "Sdk", "Sdk.props");
            Manifest = Path.Combine(Sdk.Root, "sdk-manifests", Sdk.FeatureBand, "isoplane", version, "WorkloadManifest.json");
            Marker = Path.Combine(Sdk.Root, "metadata", "workloads", Sdk.FeatureBand, "InstalledWorkloads", "isoplane");
            Write(Props, "<Project />");
            if (windows) Installation.Record(new(InstalledResourceKind.WindowsInstaller, "test-product", true, version));
            else
            {
                Write(Manifest, JsonSerializer.Serialize(new
                {
                    version,
                    packs = new Dictionary<string, object> { ["Test.Dependency"] = new { kind = "sdk", version } }
                }));
                Write(Path.Combine(Sdk.Root, "packs", "Test.Dependency", version, "Sdk", "Sdk.props"), "<Project />");
                Write(Path.ChangeExtension(Manifest, "targets"), "<Project />");
                Write(Marker, "");
            }
        }

        internal void Check() => EnvironmentDoctor.ValidateBackend(Installation, Installed, Sdk, windows);
        internal IEnumerable<string> Files() => Directory.GetFiles(Root, "*", SearchOption.AllDirectories)
            .Order().Select(path => path + File.ReadAllText(path)).ToArray();
        private static void Write(string path, string content)
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.WriteAllText(path, content);
        }
        public void Dispose() => directory.Delete(true);
    }
}
