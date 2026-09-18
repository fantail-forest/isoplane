using System.Reflection;
using System.Security.Cryptography;
using System.Text.Json;

namespace Wisp.Cli;

/// <summary>The release a CLI build belongs to: its package version and the source revision it was built from.</summary>
internal sealed record ReleaseIdentity(string Version, string SourceRevision)
{
    /// <summary>Reads the identity the running CLI was packed with from its assembly metadata.</summary>
    internal static ReleaseIdentity OfRunningCli()
    {
        var informational = typeof(ReleaseIdentity).Assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion
            ?? throw new InvalidOperationException("The CLI package is missing its version metadata; reinstall a complete package.");
        return Parse(informational);
    }

    internal static ReleaseIdentity Parse(string informationalVersion)
    {
        var separator = informationalVersion.IndexOf('+');
        if (separator <= 0 || separator == informationalVersion.Length - 1)
            throw new InvalidOperationException("The CLI package carries no source revision; reinstall it from a complete release.");
        return new(informationalVersion[..separator], informationalVersion[(separator + 1)..]);
    }
}

internal sealed record ReleaseToolchain(string Directory, string ManifestSha256);
internal sealed record ReleasePackage(string Id, string Version, string File, string Sha256);
internal sealed record ReleaseManifest(string SourceRevision, string PackageVersion, string RuntimeIdentifier, ReleaseToolchain Toolchain, ReleasePackage[] Packages);

/// <summary>A preview release directory: <c>release.json</c> naming the source revision, the package version and the toolchain it ships.</summary>
internal static class Release
{
    internal const string ManifestName = "release.json";
    private static readonly JsonSerializerOptions Json = new() { PropertyNameCaseInsensitive = true };

    /// <summary>
    /// Installs the toolchain of the release at <paramref name="directory"/> after proving the release is the one
    /// <paramref name="cli"/> was built from. Fails before touching the installation on a missing or altered manifest,
    /// a different version or revision, or a toolchain the manifest does not describe.
    /// </summary>
    internal static async Task<string> InstallAsync(string directory, ReleaseIdentity cli, string home, CancellationToken cancellation, string? bundledRuntime = null)
    {
        directory = Path.GetFullPath(directory);
        var manifest = await ReadAsync(directory, cancellation);
        RequireSameRelease(manifest, cli, directory);
        var toolchain = Toolchain.Resolve(directory, manifest.Toolchain.Directory);
        var toolchainManifest = Path.Combine(toolchain, Toolchain.ManifestName);
        if (!File.Exists(toolchainManifest))
            throw new InvalidOperationException($"The release at {directory} has no toolchain at {manifest.Toolchain.Directory}; extract the complete release and rerun setup.");
        await using (var stream = File.OpenRead(toolchainManifest))
        {
            var digest = Convert.ToHexStringLower(await SHA256.HashDataAsync(stream, cancellation));
            if (!string.Equals(digest, manifest.Toolchain.ManifestSha256, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException($"The toolchain at {toolchain} is not the one {ManifestName} describes; the release is incomplete or altered. Extract the complete release again and rerun setup.");
        }
        return await Toolchain.InstallAsync(toolchain, home, cancellation, bundledRuntime);
    }

    internal static async Task<ReleaseManifest> ReadAsync(string directory, CancellationToken cancellation)
    {
        var path = Path.Combine(directory, ManifestName);
        if (!File.Exists(path))
            throw new InvalidOperationException($"No {ManifestName} at {directory}. Point --source at the extracted release directory, which contains {ManifestName}, packages/ and toolchain/.");
        var manifest = JsonSerializer.Deserialize<ReleaseManifest>(await File.ReadAllBytesAsync(path, cancellation), Json)
            ?? throw new InvalidOperationException($"{path} is empty; extract the complete release again.");
        if (string.IsNullOrWhiteSpace(manifest.SourceRevision) || string.IsNullOrWhiteSpace(manifest.PackageVersion)
            || string.IsNullOrWhiteSpace(manifest.RuntimeIdentifier) || manifest.Toolchain is null
            || string.IsNullOrWhiteSpace(manifest.Toolchain.Directory) || manifest.Toolchain.ManifestSha256 is not { Length: 64 }
            || manifest.Packages is not { Length: > 0 })
            throw new InvalidOperationException($"{path} does not identify the release's revision, version, packages and toolchain; extract the complete release again.");
        return manifest;
    }

    private static void RequireSameRelease(ReleaseManifest manifest, ReleaseIdentity cli, string directory)
    {
        if (manifest.PackageVersion == cli.Version && manifest.SourceRevision == cli.SourceRevision)
            return;
        throw new InvalidOperationException(
            $"isoplane {cli.Version} (revision {Short(cli.SourceRevision)}) cannot install the release at {directory}: "
            + $"it is version {manifest.PackageVersion} (revision {Short(manifest.SourceRevision)}). Install the CLI from that release first: "
            + $"dotnet tool update --global Isoplane.Cli --version {manifest.PackageVersion} --add-source {Path.Combine(directory, "packages")}");
    }

    private static string Short(string revision) => revision.Length > 12 ? revision[..12] : revision;
}
