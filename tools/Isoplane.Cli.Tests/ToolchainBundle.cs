using System.Security.Cryptography;
using System.Text.Json;

namespace Wisp.Cli.Tests;

/// <summary>A synthetic toolchain directory whose manifest lists every required file, laid out as a release ships it.</summary>
internal sealed class ToolchainBundle : IDisposable
{
    internal string Root { get; } = Directory.CreateTempSubdirectory("wisp-toolchain-test-").FullName;
    internal string Release => Path.Combine(Root, "release");
    internal string Source => Path.Combine(Release, "toolchain");
    internal string Home => Path.Combine(Root, "home");

    internal ToolchainBundle(string path = "bin/tool", string? platform = null)
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

    /// <summary>Writes the release manifest describing this toolchain, as the release build does.</summary>
    internal ReleaseManifest DescribeRelease(string version, string revision)
    {
        var manifest = new ReleaseManifest(revision, version, Toolchain.PlatformIdentifier(),
            new ReleaseToolchain("toolchain", Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(Path.Combine(Source, "toolchain.json"))))),
            [new ReleasePackage("Isoplane.Cli", version, $"packages/Isoplane.Cli.{version}.nupkg", new string('0', 64))]);
        File.WriteAllText(Path.Combine(Release, Wisp.Cli.Release.ManifestName), JsonSerializer.Serialize(manifest));
        return manifest;
    }

    public void Dispose() => Directory.Delete(Root, recursive: true);
}
