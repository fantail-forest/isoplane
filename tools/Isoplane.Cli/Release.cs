using System.Reflection;
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

    /// <summary>The release directory name, which is also its path under a release origin.</summary>
    internal string ReleaseName => Release.Name(Version, Toolchain.PlatformIdentifier());
}

internal sealed record ReleaseToolchain(string Archive, string ArchiveSha256, string ManifestSha256);
internal sealed record ReleasePackage(string Id, string Version, string File, string Sha256);
internal sealed record ReleaseManifest(string SourceRevision, string PackageVersion, string RuntimeIdentifier, ReleaseToolchain Toolchain, ReleasePackage[] Packages);

/// <summary>A preview release: <c>release.json</c> naming the source revision, the package version, the packages and the toolchain archive it ships.</summary>
internal static class Release
{
    internal const string ManifestName = "release.json";
    /// <summary>The origin releases are published under; a release lives at <c>&lt;origin&gt;/&lt;release name&gt;/</c>.</summary>
    internal const string DefaultOrigin = "https://releases.isoplane.dev/";
    internal const string OriginVariable = "ISOPLANE_RELEASE_ORIGIN";
    private static readonly JsonSerializerOptions Json = new() { PropertyNameCaseInsensitive = true };

    internal static string Name(string version, string runtimeIdentifier) => $"isoplane-{runtimeIdentifier}-{version}";

    /// <summary>The origin setup downloads from: the environment override, else the published origin.</summary>
    internal static string Origin()
    {
        var configured = Environment.GetEnvironmentVariable(OriginVariable);
        return string.IsNullOrWhiteSpace(configured) ? DefaultOrigin : configured;
    }

    /// <summary>Reads and validates the manifest that <paramref name="source"/> serves.</summary>
    internal static async Task<ReleaseManifest> ReadAsync(ReleaseSource source, CancellationToken cancellation)
    {
        var bytes = await source.ReadAsync(ManifestName, cancellation);
        var location = source.Describe(ManifestName);
        ReleaseManifest? manifest;
        try
        {
            manifest = JsonSerializer.Deserialize<ReleaseManifest>(bytes, Json);
        }
        catch (JsonException error)
        {
            throw new InvalidOperationException($"{location} is not a release manifest: {error.Message}", error);
        }
        if (manifest is null)
            throw new InvalidOperationException($"{location} is empty; the release is incomplete.");
        if (string.IsNullOrWhiteSpace(manifest.SourceRevision) || string.IsNullOrWhiteSpace(manifest.PackageVersion)
            || string.IsNullOrWhiteSpace(manifest.RuntimeIdentifier) || manifest.Toolchain is null
            || string.IsNullOrWhiteSpace(manifest.Toolchain.Archive) || !IsDigest(manifest.Toolchain.ArchiveSha256) || !IsDigest(manifest.Toolchain.ManifestSha256)
            || manifest.Packages is not { Length: > 0 }
            || manifest.Packages.Any(package => package is null || string.IsNullOrWhiteSpace(package.Id) || string.IsNullOrWhiteSpace(package.File) || !IsDigest(package.Sha256)))
            throw new InvalidOperationException($"{location} does not identify the release's revision, version, packages and toolchain archive; the release is incomplete.");
        Toolchain.Resolve(Path.GetTempPath(), manifest.Toolchain.Archive);
        foreach (var package in manifest.Packages)
            Toolchain.Resolve(Path.GetTempPath(), package.File);
        return manifest;
    }

    private static bool IsDigest(string? value) => value is { Length: 64 } && value.All(Uri.IsHexDigit);

    /// <summary>Refuses a release the running CLI was not built from, naming the command that installs the matching CLI.</summary>
    internal static void RequireSameRelease(ReleaseManifest manifest, ReleaseIdentity cli, ReleaseSource source)
    {
        if (manifest.PackageVersion == cli.Version && manifest.SourceRevision == cli.SourceRevision)
            return;
        var feed = source is DirectoryReleaseSource directory ? $" --add-source {Path.Combine(directory.Directory, "packages")}" : "";
        throw new InvalidOperationException(
            $"isoplane {cli.Version} (revision {Short(cli.SourceRevision)}) cannot install the release at {source.Location}: "
            + $"it is version {manifest.PackageVersion} (revision {Short(manifest.SourceRevision)}). Install the CLI from that release first: "
            + $"dotnet tool update --global Isoplane.Cli --version {manifest.PackageVersion}{feed}");
    }

    /// <summary>Refuses a release built for another platform before anything is downloaded.</summary>
    internal static void RequireThisPlatform(ReleaseManifest manifest, ReleaseSource source)
    {
        var platform = Toolchain.PlatformIdentifier();
        if (manifest.RuntimeIdentifier != platform)
            throw new InvalidOperationException($"The release at {source.Location} targets {manifest.RuntimeIdentifier}; this machine is {platform}. Isoplane releases are not available for it.");
    }

    private static string Short(string revision) => revision.Length > 12 ? revision[..12] : revision;
}
