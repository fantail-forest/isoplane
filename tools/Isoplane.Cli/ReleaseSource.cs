using System.Net;
using System.Security.Cryptography;

namespace Isoplane.Cli;

/// <summary>
/// Where a release's manifest and assets are read from: an extracted release directory or an HTTP origin.
/// Asset copies are checked against the SHA-256 supplied by the release manifest.
/// </summary>
internal abstract class ReleaseSource
{
    /// <summary>Where this source reads from, for diagnostics.</summary>
    internal abstract string Location { get; }

    internal abstract string Describe(string relative);

    /// <summary>Reads a small file such as the manifest.</summary>
    internal abstract Task<byte[]> ReadAsync(string relative, CancellationToken cancellation);

    /// <summary>Copies an asset into <paramref name="destination"/> and returns its SHA-256.</summary>
    protected abstract Task<string> CopyAsync(string relative, Stream destination, CancellationToken cancellation);

    /// <summary>A directory containing <c>release.json</c>, or an absolute http(s) URL of one.</summary>
    internal static ReleaseSource Open(string location, HttpClient client)
    {
        if (Uri.TryCreate(location, UriKind.Absolute, out var uri) && uri.Scheme is "http" or "https")
            return new HttpReleaseSource(new UriBuilder(uri) { Path = uri.AbsolutePath.TrimEnd('/') + "/" }.Uri, client);
        return new DirectoryReleaseSource(location);
    }

    /// <summary>
    /// Ensures <paramref name="destination"/> holds the asset with the expected digest, reusing an existing verified copy and
    /// otherwise copying through a temporary file so a failed or truncated copy never appears as the asset.
    /// </summary>
    internal async Task<bool> FetchAsync(string relative, string destination, string expectedSha256, CancellationToken cancellation)
    {
        if (File.Exists(destination) && await DigestAsync(destination, cancellation) == expectedSha256)
            return false;
        Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
        var temporary = destination + ".part";
        try
        {
            string actual;
            await using (var stream = new FileStream(temporary, FileMode.Create, FileAccess.Write, FileShare.None))
                actual = await CopyAsync(relative, stream, cancellation);
            if (actual != expectedSha256)
                throw new InvalidOperationException($"{Describe(relative)} does not match the release manifest (SHA-256 {actual[..12]}…, expected {expectedSha256[..12]}…); the release is incomplete or altered.");
            File.Move(temporary, destination, overwrite: true);
            return true;
        }
        finally
        {
            if (File.Exists(temporary))
                File.Delete(temporary);
        }
    }

    internal static async Task<string> DigestAsync(string path, CancellationToken cancellation)
    {
        await using var stream = File.OpenRead(path);
        return Convert.ToHexStringLower(await SHA256.HashDataAsync(stream, cancellation));
    }

    protected static async Task<string> CopyHashingAsync(Stream input, Stream destination, long? total, CancellationToken cancellation)
    {
        using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
        var buffer = new byte[1 << 16];
        long copied = 0;
        int read;
        while ((read = await input.ReadAsync(buffer, cancellation)) > 0)
        {
            hash.AppendData(buffer, 0, read);
            await destination.WriteAsync(buffer.AsMemory(0, read), cancellation);
            copied += read;
        }
        if (total is not null && copied != total)
            throw new IOException($"The copy ended after {copied} of {total} bytes.");
        return Convert.ToHexStringLower(hash.GetHashAndReset());
    }
}

internal sealed class DirectoryReleaseSource(string directory) : ReleaseSource
{
    internal string Directory { get; } = Path.GetFullPath(directory);

    internal override string Location => Directory;

    internal override string Describe(string relative) => CliPaths.Resolve(Directory, relative);

    internal override async Task<byte[]> ReadAsync(string relative, CancellationToken cancellation)
    {
        var path = CliPaths.Resolve(Directory, relative);
        if (!File.Exists(path))
            throw new InvalidOperationException($"No {relative} at {Directory}. Point --source at the extracted release directory, which contains {Release.ManifestName} and packages/.");
        return await File.ReadAllBytesAsync(path, cancellation);
    }

    protected override async Task<string> CopyAsync(string relative, Stream destination, CancellationToken cancellation)
    {
        var path = CliPaths.Resolve(Directory, relative);
        if (!File.Exists(path))
            throw new InvalidOperationException($"The release at {Directory} has no {relative}; extract the complete release and rerun setup.");
        await using var input = File.OpenRead(path);
        return await CopyHashingAsync(input, destination, input.Length, cancellation);
    }
}

internal sealed class HttpReleaseSource(Uri release, HttpClient client) : ReleaseSource
{
    internal override string Location => release.AbsoluteUri;

    internal override string Describe(string relative) => new Uri(release, relative).AbsoluteUri;

    internal override async Task<byte[]> ReadAsync(string relative, CancellationToken cancellation)
    {
        using var response = await GetAsync(relative, cancellation);
        return await response.Content.ReadAsByteArrayAsync(cancellation);
    }

    protected override async Task<string> CopyAsync(string relative, Stream destination, CancellationToken cancellation)
    {
        using var response = await GetAsync(relative, cancellation);
        await using var input = await response.Content.ReadAsStreamAsync(cancellation);
        try
        {
            return await CopyHashingAsync(input, destination, response.Content.Headers.ContentLength, cancellation);
        }
        catch (Exception error) when (error is HttpRequestException or IOException)
        {
            throw new InvalidOperationException($"The download of {Describe(relative)} was interrupted: {error.Message}. Check the connection and rerun isoplane setup; verified downloads are kept.", error);
        }
    }

    private async Task<HttpResponseMessage> GetAsync(string relative, CancellationToken cancellation)
    {
        var url = new Uri(release, CliPaths.Resolve("/", relative)[1..]);
        HttpResponseMessage response;
        try
        {
            response = await client.GetAsync(url, HttpCompletionOption.ResponseHeadersRead, cancellation);
        }
        catch (HttpRequestException error)
        {
            throw new InvalidOperationException($"Cannot download {url}: {error.Message}. Check the connection, or point --source at an extracted release directory.", error);
        }
        if (response.StatusCode == HttpStatusCode.NotFound)
        {
            response.Dispose();
            throw new InvalidOperationException($"{url} is not published (HTTP 404). The release the installed CLI belongs to is unavailable at this origin; check --source or point it at a local release directory.");
        }
        if (!response.IsSuccessStatusCode)
        {
            var status = (int)response.StatusCode;
            response.Dispose();
            throw new InvalidOperationException($"Cannot download {url}: HTTP {status}. Retry later, or point --source at an extracted release directory.");
        }
        return response;
    }
}
