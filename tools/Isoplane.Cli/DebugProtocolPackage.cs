using System.IO.Compression;
using System.Security.Cryptography;
using Isoplane.Debugging;

namespace Isoplane.Cli;

/// <summary>
/// The package setup downloads Microsoft's Debug Adapter Protocol library from, and the digests its file and the
/// assembly inside it must have. Tests substitute a package of their own.
/// </summary>
internal sealed record DebugProtocolPackage(string Version, Uri Download, string Sha256, string AssemblySha256)
{
    /// <summary>The package the debugger was compiled against, as nuget.org serves it.</summary>
    internal static DebugProtocolPackage Pinned()
    {
        var id = DebugProtocolLibrary.PackageId.ToLowerInvariant();
        var version = DebugProtocolLibrary.PackageVersion;
        return new(version, new Uri($"https://api.nuget.org/v3-flatcontainer/{id}/{version}/{id}.{version}.nupkg"),
            DebugProtocolLibrary.PackageSha256, DebugProtocolLibrary.AssemblySha256);
    }

    internal string Describe() => $"Microsoft's {DebugProtocolLibrary.PackageId} {Version} library";

    /// <summary>Whether the installation at <paramref name="home"/> already holds this package's assembly.</summary>
    internal bool IsInstalledIn(string home)
    {
        var path = DebugProtocolLibrary.PathIn(home);
        if (!File.Exists(path))
            return false;
        using var stream = File.OpenRead(path);
        return Convert.ToHexStringLower(SHA256.HashData(stream)) == AssemblySha256;
    }

    /// <summary>
    /// Downloads the package and returns the one assembly the debugger loads. Fails with
    /// <see cref="DebugProtocolPackageException"/> when the download fails or either digest differs.
    /// </summary>
    internal async Task<byte[]> FetchAssemblyAsync(HttpClient client, CancellationToken cancellation)
    {
        byte[] package;
        try
        {
            package = await client.GetByteArrayAsync(Download, cancellation);
        }
        catch (Exception error) when (error is HttpRequestException or IOException)
        {
            throw new DebugProtocolPackageException($"Cannot download {Download}: {error.Message}. Nothing was installed. Check the connection and rerun isoplane setup.", error);
        }
        // The digest is checked before the archive is read, so only the expected package is ever parsed.
        Require(package, Sha256, Download.AbsoluteUri);
        using var archive = new ZipArchive(new MemoryStream(package), ZipArchiveMode.Read);
        var entry = archive.GetEntry(DebugProtocolLibrary.AssemblyEntry)
            ?? throw new DebugProtocolPackageException($"{Download} has no {DebugProtocolLibrary.AssemblyEntry}. Nothing was installed.");
        using var assembly = new MemoryStream();
        await using (var content = entry.Open())
            await content.CopyToAsync(assembly, cancellation);
        Require(assembly.ToArray(), AssemblySha256, $"{DebugProtocolLibrary.AssemblyEntry} in {Download}");
        return assembly.ToArray();
    }

    private static void Require(byte[] content, string expectedSha256, string described)
    {
        var actual = Convert.ToHexStringLower(SHA256.HashData(content));
        if (actual != expectedSha256)
            throw new DebugProtocolPackageException($"{described} is not the file this CLI expects (SHA-256 {actual[..12]}…, expected {expectedSha256[..12]}…). Nothing was installed.");
    }
}

/// <summary>Thrown when Microsoft's Debug Adapter Protocol package cannot be downloaded or is not the pinned one.</summary>
internal sealed class DebugProtocolPackageException(string message, Exception? inner = null) : InvalidOperationException(message, inner);
