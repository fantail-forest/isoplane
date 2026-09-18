using System.Formats.Tar;
using System.IO.Compression;
using System.Security.Cryptography;
using System.Text.Json;

namespace Isoplane.Cli.Tests;

/// <summary>
/// A synthetic release: a toolchain directory whose manifest lists every required file, archived as a release ships
/// it, beside a package feed and a release manifest. <see cref="Archive"/> re-packs the toolchain after edits.
/// </summary>
internal sealed class ToolchainBundle : IDisposable
{
    internal const string Version = "0.1.0-preview.1";
    internal const string Revision = "0123456789abcdef0123456789abcdef01234567";
    internal static readonly string[] PackageIds = ["Isoplane.Cli", "Isoplane.Native", "Isoplane.Testing", "Isoplane.Sdk", "Isoplane.AspNetCore", "Isoplane.Templates"];

    internal string Root { get; } = Directory.CreateTempSubdirectory("isoplane-toolchain-test-").FullName;
    internal string Release => Path.Combine(Root, "release");
    internal string Source => Path.Combine(Root, "toolchain");
    internal string Home => Path.Combine(Root, "home");
    internal string UserHome => Path.Combine(Root, "user");
    internal string ArchivePath => Path.Combine(Release, "toolchain.tgz");
    internal ReleaseIdentity Cli { get; }

    internal ToolchainBundle(string path = "bin/tool", string? platform = null, string version = Version, string revision = Revision)
    {
        Cli = new(version, revision);
        Directory.CreateDirectory(Path.Combine(Source, "bin"));
        Directory.CreateDirectory(Path.Combine(Release, "packages"));
        Directory.CreateDirectory(UserHome);
        File.WriteAllText(Path.Combine(UserHome, ".profile"), "# user profile\n");
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
        foreach (var id in PackageIds)
            File.WriteAllText(Path.Combine(Release, "packages", $"{id}.{version}.nupkg"), $"{id} {version} {revision}");
        DescribeRelease(version, revision);
    }

    /// <summary>The identity of the toolchain as authored: the SHA-256 of its manifest.</summary>
    internal string Identity => Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(Path.Combine(Source, "toolchain.json"))));

    /// <summary>Packs the toolchain directory into the release's archive, as the release build does.</summary>
    internal string Archive()
    {
        using (var stream = File.Create(ArchivePath))
        using (var compressed = new GZipStream(stream, CompressionLevel.Fastest))
            TarFile.CreateFromDirectory(Source, compressed, includeBaseDirectory: false);
        return ArchivePath;
    }

    /// <summary>Writes the release manifest describing the archived toolchain and the packages, as the release build does.</summary>
    internal ReleaseManifest DescribeRelease(string version, string revision)
    {
        Archive();
        var packages = PackageIds.Select(id =>
        {
            var file = $"packages/{id}.{version}.nupkg";
            var path = Path.Combine(Release, file);
            if (!File.Exists(path))
                File.WriteAllText(path, $"{id} {version} {revision}");
            return new ReleasePackage(id, version, file, Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(path))));
        }).ToArray();
        var manifest = new ReleaseManifest(revision, version, Toolchain.PlatformIdentifier(),
            new ReleaseToolchain("toolchain.tgz", Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(ArchivePath))), Identity), packages);
        File.WriteAllText(Path.Combine(Release, Isoplane.Cli.Release.ManifestName), JsonSerializer.Serialize(manifest));
        return manifest;
    }

    /// <summary>Installs and selects the archived toolchain directly, the way setup does after its checks.</summary>
    internal async Task<string> InstallAsync(string? bundledRuntime = null)
    {
        var installed = await Toolchain.InstallArchiveAsync(Archive(), Identity, Home, CancellationToken.None, bundledRuntime);
        await Toolchain.SelectAsync(Home, installed, CancellationToken.None);
        return installed;
    }

    public void Dispose() => Directory.Delete(Root, recursive: true);
}

/// <summary>Registrations kept in memory, recording every call.</summary>
internal sealed class FakeRegistrations : ISdkRegistrations
{
    internal Dictionary<string, string> Sources { get; } = new(StringComparer.Ordinal);
    internal Dictionary<string, string> Templates { get; } = new(StringComparer.Ordinal);
    internal List<string> Calls { get; } = [];
    internal Func<string, Exception?>? FailUninstallTemplate { get; set; }

    public Task<string?> PackageSourceAsync(string name, CancellationToken cancellation) => Task.FromResult(Sources.GetValueOrDefault(name));

    public Task AddPackageSourceAsync(string name, string path, CancellationToken cancellation)
    {
        Calls.Add($"add source {name}");
        Sources[name] = path;
        return Task.CompletedTask;
    }

    public Task RemovePackageSourceAsync(string name, CancellationToken cancellation)
    {
        Calls.Add($"remove source {name}");
        Sources.Remove(name);
        return Task.CompletedTask;
    }

    public Task<string?> TemplateVersionAsync(string packageId, CancellationToken cancellation) => Task.FromResult(Templates.GetValueOrDefault(packageId));

    public Task InstallTemplateAsync(string package, CancellationToken cancellation)
    {
        Calls.Add($"install template {Path.GetFileName(package)}");
        var name = Path.GetFileNameWithoutExtension(package);
        var separator = name.IndexOf('.', name.IndexOf('.') + 1);
        Templates[name[..separator]] = name[(separator + 1)..];
        return Task.CompletedTask;
    }

    public Task UninstallTemplateAsync(string packageId, CancellationToken cancellation)
    {
        if (FailUninstallTemplate?.Invoke(packageId) is { } failure)
            throw failure;
        Calls.Add($"uninstall template {packageId}");
        Templates.Remove(packageId);
        return Task.CompletedTask;
    }
}

/// <summary>A machine on which every prerequisite is present, nobody is asked anything, and the installed tools are taken to run.</summary>
internal static class Machines
{
    internal static SetupMachine Ready(ToolchainBundle bundle, FakeRegistrations registrations) => new(
        _ => true, _ => true, new OperatingSystemRelease("ubuntu", "24.04"),
        _ => throw new InvalidOperationException("nothing should be asked on a ready machine"),
        (_, _) => throw new InvalidOperationException("nothing privileged should run on a ready machine"),
        _ => registrations, (_, _) => Task.CompletedTask, bundle.UserHome, null);
}

/// <summary>An HTTP origin serving a directory tree, with files that can be withheld or cut short.</summary>
internal sealed class StaticOriginHandler(string root) : HttpMessageHandler
{
    internal HashSet<string> Missing { get; } = [];
    internal HashSet<string> Truncated { get; } = [];
    internal List<string> Requests { get; } = [];

    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellation)
    {
        var relative = request.RequestUri!.AbsolutePath.TrimStart('/');
        Requests.Add(relative);
        var path = Path.Combine(root, relative);
        if (Missing.Contains(relative) || !File.Exists(path))
            return Task.FromResult(new HttpResponseMessage(System.Net.HttpStatusCode.NotFound));
        var bytes = File.ReadAllBytes(path);
        var content = new ByteArrayContent(Truncated.Contains(relative) ? bytes[..(bytes.Length / 2)] : bytes);
        content.Headers.ContentLength = bytes.Length;
        return Task.FromResult(new HttpResponseMessage(System.Net.HttpStatusCode.OK) { Content = content });
    }
}
