using System.Security.Cryptography;
using System.Text.Json;
using Xunit;

namespace Wisp.Cli.Tests;

public sealed class ToolchainTests
{
    [Fact]
    public async Task SetupRefusesAnUntrackedCompilerPackage()
    {
        using var fixture = new Bundle();
        var path = Path.Combine(fixture.Source, "toolchain.json");
        var manifest = JsonSerializer.Deserialize<ToolchainManifest>(await File.ReadAllTextAsync(path))!;
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(manifest with
        {
            Files = manifest.Files.Where(file => file.Path != "compiler/fixture.nupkg").ToArray()
        }));
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None));
        Assert.True(error.Message.Contains("compiler package", StringComparison.Ordinal), "setup must diagnose an incomplete compiler payload");
    }

    [Fact]
    public void DoctorExplainsAnUnloadableCompilerBackend()
    {
        using var fixture = new Bundle();
        var failure = Assert.Throws<InvalidOperationException>(() => Toolchain.ProbeCompiler(fixture.Source));
        Assert.True(failure.Message.Contains("native dependencies", StringComparison.Ordinal), "doctor must explain missing native compiler dependencies");
    }

    [Fact]
    public async Task Setup_installs_relocatable_files_and_reuses_the_verified_installation()
    {
        using var fixture = new Bundle();
        var first = await Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None);
        Directory.Delete(fixture.Source, recursive: true);
        Assert.Equal("payload", await File.ReadAllTextAsync(Path.Combine(first, "bin", "tool")));
        Assert.True(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None) == first,
            "installed files must remain usable after deleting their source");
        Assert.DoesNotContain(fixture.Source, await File.ReadAllTextAsync(Path.Combine(fixture.Home, "toolchain.props")));
    }

    [Fact]
    public async Task Setup_can_be_rerun_without_replacing_a_valid_installation()
    {
        using var fixture = new Bundle();
        var first = await Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None);
        var second = await Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None);
        Assert.True(first == second, "identical content must reuse the installed toolchain identity");
        Assert.True(new DirectoryInfo(Path.Combine(fixture.Home, "dotnet")).ResolveLinkTarget(true)?.FullName == Path.Combine(first, "dotnet"),
            "the stable SDK path must resolve to the selected toolchain");
    }

    [Fact]
    public async Task Corrupt_source_never_becomes_the_selected_toolchain()
    {
        using var fixture = new Bundle();
        await File.WriteAllTextAsync(Path.Combine(fixture.Source, "bin", "tool"), "corrupt");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None));
        Assert.False(File.Exists(Path.Combine(fixture.Home, "current.json")), "corrupt files must not be activated");
        Assert.Empty(Directory.GetDirectories(Path.Combine(fixture.Home, "toolchains")));
    }

    [Fact]
    public async Task Doctor_reports_corruption_without_repairing_it()
    {
        using var fixture = new Bundle();
        var installed = await Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None);
        var path = Path.Combine(installed, "bin", "tool");
        await File.WriteAllTextAsync(path, "corrupt");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
        Assert.True(await File.ReadAllTextAsync(path) == "corrupt", "doctor must be non-mutating");
    }

    [Fact]
    public async Task A_manifest_cannot_write_outside_the_installation()
    {
        using var fixture = new Bundle("../escaped");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None));
        Assert.False(Directory.Exists(fixture.Home), "invalid paths must be refused before installation starts");
    }

    [Fact]
    public async Task A_different_platform_bundle_is_refused_before_installing()
    {
        using var fixture = new Bundle(platform: "other-platform");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None));
        Assert.False(Directory.Exists(fixture.Home), "incompatible binaries must not be installed");
    }

    [Fact]
    public async Task Source_symlinks_cannot_import_unlisted_external_files()
    {
        using var fixture = new Bundle();
        var path = Path.Combine(fixture.Source, "bin", "tool");
        var external = Path.Combine(fixture.Root, "external");
        File.Move(path, external);
        File.CreateSymbolicLink(path, external);
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None));
    }

    [Fact]
    public async Task An_incomplete_bundle_cannot_be_installed()
    {
        using var fixture = new Bundle();
        var manifestPath = Path.Combine(fixture.Source, "toolchain.json");
        var manifest = JsonSerializer.Deserialize<ToolchainManifest>(await File.ReadAllTextAsync(manifestPath))!;
        await File.WriteAllTextAsync(manifestPath, JsonSerializer.Serialize(manifest with { Files = manifest.Files.Where(file => file.Path != "bin/wisp").ToArray() }));
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None));
        Assert.False(Directory.Exists(fixture.Home), "missing engine entries must be rejected before installation starts");
    }

    [Fact]
    public async Task Setup_repairs_corrupt_installed_files_from_a_verified_bundle()
    {
        using var fixture = new Bundle();
        var installed = await Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None);
        await File.WriteAllTextAsync(Path.Combine(installed, "bin", "tool"), "corrupt");
        await Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None);
        Assert.True(await Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None) == installed,
            "setup must repair the selected installation without manual filesystem cleanup");
    }

    [Fact]
    public async Task Child_builds_use_the_selected_sdk_and_runtime_feed()
    {
        using var fixture = new Bundle();
        var installed = await Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None);
        var start = new System.Diagnostics.ProcessStartInfo("dotnet");
        Toolchain.Apply(start, fixture.Home);
        Assert.True(start.FileName == Path.Combine(installed, "dotnet", "dotnet"), "publication must use the selected SDK");
        Assert.True(start.Environment["WISP_GUEST_RUNTIME_FEED"] == Path.Combine(installed, "feed"), "publication must use the selected runtime packs");
    }

    [Fact]
    public async Task A_toolchain_from_another_cli_release_is_refused_before_installation()
    {
        using var fixture = new Bundle();
        var bundled = Path.Combine(fixture.Root, "runtime");
        Directory.CreateDirectory(bundled);
        await File.WriteAllTextAsync(Path.Combine(bundled, "wisp"), "different engine");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None, bundled));
        Assert.False(Directory.Exists(fixture.Home), "mixed release components must not replace a working installation");
    }

    [Fact]
    public async Task Doctor_detects_a_broken_msbuild_selection_without_repairing_it()
    {
        using var fixture = new Bundle();
        await Toolchain.InstallAsync(fixture.Source, fixture.Home, CancellationToken.None);
        var props = Path.Combine(fixture.Home, "toolchain.props");
        await File.WriteAllTextAsync(props, "<Project />");
        await Assert.ThrowsAsync<InvalidOperationException>(() => Toolchain.DiagnoseAsync(fixture.Home, CancellationToken.None));
        Assert.True(await File.ReadAllTextAsync(props) == "<Project />", "doctor must not rewrite inconsistent selections");
    }

    private sealed class Bundle : IDisposable
    {
        internal string Root { get; } = Directory.CreateTempSubdirectory("wisp-toolchain-test-").FullName;
        internal string Source => Path.Combine(Root, "source");
        internal string Home => Path.Combine(Root, "home");

        internal Bundle(string path = "bin/tool", string? platform = null)
        {
            Directory.CreateDirectory(Path.Combine(Source, "bin"));
            File.WriteAllText(Path.Combine(Source, "bin", "tool"), "payload");
            var digest = Convert.ToHexStringLower(SHA256.HashData("payload"u8));
            var entries = new List<ToolchainFile> { new(path, digest, false) };
            foreach (var required in Toolchain.RequiredFiles("10.0.400", "10.0.11", platform ?? Toolchain.PlatformIdentifier()))
            {
                var target = Path.Combine(Source, required);
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                var content = required == "compiler/compiler.json"
                    ? JsonSerializer.Serialize(new { package = "fixture.nupkg", package_sha256 = digest, backend_sha256 = digest })
                    : "payload";
                File.WriteAllText(target, content);
                entries.Add(new(required, Convert.ToHexStringLower(SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(content))), false));
            }
            File.WriteAllText(Path.Combine(Source, "compiler", "fixture.nupkg"), "payload");
            entries.Add(new("compiler/fixture.nupkg", digest, false));
            var manifest = new ToolchainManifest(platform ?? Toolchain.PlatformIdentifier(), "10.0.400", "10.0.11", entries.ToArray());
            File.WriteAllText(Path.Combine(Source, "toolchain.json"), JsonSerializer.Serialize(manifest));
        }

        public void Dispose() => Directory.Delete(Root, recursive: true);
    }
}
