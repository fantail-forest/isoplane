using System.Security.Cryptography;
using System.Text.Json;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class ToolchainTests
{
    [Fact]
    public async Task SetupRefusesAnUntrackedCompilerPackage()
    {
        using var fixture = new ToolchainBundle();
        var path = Path.Combine(fixture.Source, "toolchain.json");
        var manifest = JsonSerializer.Deserialize<ToolchainManifest>(await File.ReadAllTextAsync(path))!;
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(manifest with
        {
            Files = manifest.Files.Where(file => file.Path != "compiler/fixture.nupkg").ToArray()
        }));
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.InstallAsync());
        Assert.True(error.Message.Contains("compiler package", StringComparison.Ordinal), "setup must diagnose an incomplete compiler payload");
    }

    [Fact]
    public void DoctorExplainsAnUnloadableCompilerBackend()
    {
        using var fixture = new ToolchainBundle();
        var failure = Assert.Throws<InvalidOperationException>(() => Toolchain.ProbeCompiler(fixture.Source));
        Assert.True(failure.Message.Contains("native dependencies", StringComparison.Ordinal), "doctor must explain missing native compiler dependencies");
    }

    private static void AssertNothingInstalled(ToolchainBundle fixture, string because)
    {
        Assert.True(!Directory.Exists(Toolchain.Installations(fixture.Home)) || Directory.GetDirectories(Toolchain.Installations(fixture.Home)).Length == 0, because);
        Assert.False(File.Exists(Path.Combine(fixture.Home, "current.json")), because);
    }

    [Fact]
    public async Task Setup_installs_relocatable_files_and_reuses_the_verified_installation()
    {
        using var fixture = new ToolchainBundle();
        var first = await fixture.InstallAsync();
        Directory.Delete(fixture.Source, recursive: true);
        File.Delete(fixture.ArchivePath);
        Assert.Equal("payload", await File.ReadAllTextAsync(Path.Combine(first, "bin", "tool")));
        Assert.True(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None) == first,
            "installed files must remain usable after deleting their source");
        Assert.DoesNotContain(fixture.Source, await File.ReadAllTextAsync(Path.Combine(fixture.Home, "toolchain.props")));
    }

    [Fact]
    public async Task Setup_can_be_rerun_without_replacing_a_valid_installation()
    {
        using var fixture = new ToolchainBundle();
        var first = await fixture.InstallAsync();
        var second = await fixture.InstallAsync();
        Assert.True(first == second, "identical content must reuse the installed toolchain identity");
        Assert.True(await Toolchain.VerifiedInstallationAsync(fixture.Home, fixture.Identity, CancellationToken.None) == first, "a verified installation is found by its identity");
        Assert.True(new DirectoryInfo(Path.Combine(fixture.Home, "dotnet")).ResolveLinkTarget(true)?.FullName == Path.Combine(first, "dotnet"),
            "the stable SDK path must resolve to the selected toolchain");
    }

    [Fact]
    public async Task Corrupt_source_never_becomes_the_selected_toolchain()
    {
        using var fixture = new ToolchainBundle();
        await File.WriteAllTextAsync(Path.Combine(fixture.Source, "bin", "tool"), "corrupt");
        await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.InstallAsync());
        Assert.False(File.Exists(Path.Combine(fixture.Home, "current.json")), "corrupt files must not be activated");
        Assert.Empty(Directory.GetDirectories(Path.Combine(fixture.Home, "toolchains")));
    }

    [Fact]
    public async Task Doctor_reports_corruption_without_repairing_it()
    {
        using var fixture = new ToolchainBundle();
        var installed = await fixture.InstallAsync();
        var path = Path.Combine(installed, "bin", "tool");
        await File.WriteAllTextAsync(path, "corrupt");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
        Assert.True(await File.ReadAllTextAsync(path) == "corrupt", "doctor must be non-mutating");
    }

    [Fact]
    public async Task A_manifest_cannot_write_outside_the_installation()
    {
        using var fixture = new ToolchainBundle("../escaped");
        await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.InstallAsync());
        AssertNothingInstalled(fixture, "invalid paths must be refused before installation starts");
    }

    [Fact]
    public async Task A_different_platform_bundle_is_refused_before_installing()
    {
        using var fixture = new ToolchainBundle(platform: "other-platform");
        await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.InstallAsync());
        AssertNothingInstalled(fixture, "incompatible binaries must not be installed");
    }

    [Fact]
    public async Task Source_symlinks_cannot_import_unlisted_external_files()
    {
        using var fixture = new ToolchainBundle();
        var path = Path.Combine(fixture.Source, "bin", "tool");
        var external = Path.Combine(fixture.Root, "external");
        File.Move(path, external);
        File.CreateSymbolicLink(path, external);
        await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.InstallAsync());
    }

    [Fact]
    public async Task An_incomplete_bundle_cannot_be_installed()
    {
        using var fixture = new ToolchainBundle();
        var manifestPath = Path.Combine(fixture.Source, "toolchain.json");
        var manifest = JsonSerializer.Deserialize<ToolchainManifest>(await File.ReadAllTextAsync(manifestPath))!;
        await File.WriteAllTextAsync(manifestPath, JsonSerializer.Serialize(manifest with { Files = manifest.Files.Where(file => file.Path != "cli/Isoplane.Cli.dll").ToArray() }));
        await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.InstallAsync());
        AssertNothingInstalled(fixture, "missing engine entries must be rejected before installation starts");
    }

    [Fact]
    public async Task Setup_repairs_corrupt_installed_files_from_a_verified_bundle()
    {
        using var fixture = new ToolchainBundle();
        var installed = await fixture.InstallAsync();
        await File.WriteAllTextAsync(Path.Combine(installed, "bin", "tool"), "corrupt");
        Assert.Null(await Toolchain.VerifiedInstallationAsync(fixture.Home, fixture.Identity, CancellationToken.None));
        await fixture.InstallAsync();
        Assert.True(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None) == installed,
            "setup must repair the selected installation without manual filesystem cleanup");
    }

    [Fact]
    public async Task Child_builds_use_the_selected_sdk_and_runtime_feed()
    {
        using var fixture = new ToolchainBundle();
        var installed = await fixture.InstallAsync();
        var start = new System.Diagnostics.ProcessStartInfo("dotnet");
        Toolchain.Apply(start, fixture.Home);
        Assert.True(start.FileName == Path.Combine(installed, "dotnet", "dotnet"), "publication must use the selected SDK");
        Assert.True(start.Environment["ISOPLANE_GUEST_RUNTIME_FEED"] == Path.Combine(installed, "feed"), "publication must use the selected runtime packs");
    }

    [Fact]
    public async Task A_toolchain_from_another_cli_release_is_refused_before_installation()
    {
        using var fixture = new ToolchainBundle();
        var bundled = Path.Combine(fixture.Root, "runtime");
        Directory.CreateDirectory(bundled);
        await File.WriteAllTextAsync(Path.Combine(bundled, Isoplane.Native.NativeRuntime.LibraryFilename), "different engine");
        await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.InstallAsync(bundled));
        AssertNothingInstalled(fixture, "mixed release components must not replace a working installation");
    }

    [Fact]
    public async Task Doctor_detects_a_broken_msbuild_selection_without_repairing_it()
    {
        using var fixture = new ToolchainBundle();
        await fixture.InstallAsync();
        var props = Path.Combine(fixture.Home, "toolchain.props");
        await File.WriteAllTextAsync(props, "<Project />");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
        Assert.True(await File.ReadAllTextAsync(props) == "<Project />", "doctor must not rewrite inconsistent selections");
    }
}
