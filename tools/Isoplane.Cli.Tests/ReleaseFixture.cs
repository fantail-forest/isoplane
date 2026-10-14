using System.Security.Cryptography;
using System.Text.Json;

namespace Isoplane.Cli.Tests;

/// <summary>
/// A synthetic package-only release for offline setup and download validation.
/// </summary>
internal sealed class ReleaseFixture : IDisposable
{
    internal const string Version = "0.1.0-preview.1";
    internal const string Revision = "0123456789abcdef0123456789abcdef01234567";
    internal static readonly string[] PackageIds = ["Isoplane.Cli", "Isoplane.Native", "Isoplane.Testing", "Isoplane.Sdk", "Isoplane.AspNetCore", "Isoplane.AspNetCore.DataProtection.EntityFrameworkCore", "Isoplane.Templates"];

    internal string Root { get; } = Directory.CreateTempSubdirectory("isoplane-release-test-").FullName;
    internal string Release => Path.Combine(Root, "release");
    internal string Source => Root;
    internal string Home => Path.Combine(Root, "home");
    internal string UserHome => Path.Combine(Root, "user");
    internal ReleaseIdentity Cli { get; }

    internal ReleaseFixture()
    {
        Cli = new(Version, Revision);
        Directory.CreateDirectory(Path.Combine(Release, "packages"));
        Directory.CreateDirectory(UserHome);
        File.WriteAllText(Path.Combine(UserHome, ".profile"), "# user profile\n");
        DescribeRelease(Version, Revision);
    }

    internal ReleaseManifest DescribeRelease(string version, string revision)
    {
        var packages = PackageIds.Select(id =>
        {
            var file = $"packages/{id}.{version}.nupkg";
            var path = Path.Combine(Release, file);
            File.WriteAllText(path, $"{id} {version} {revision}");
            return new ReleasePackage(id, version, file, Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(path))));
        }).ToArray();
        var manifest = new ReleaseManifest(revision, version, CliPaths.PlatformIdentifier(), packages);
        File.WriteAllText(Path.Combine(Release, Isoplane.Cli.Release.ManifestName), JsonSerializer.Serialize(manifest));
        return manifest;
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

    public Task InstallTemplateAsync(string source, CancellationToken cancellation)
    {
        Calls.Add($"install template {source}");
        var version = source.Contains("::", StringComparison.Ordinal)
            ? source.Split("::")[1]
            : Path.GetFileName(source)["Isoplane.Templates.".Length..^".nupkg".Length];
        Templates["Isoplane.Templates"] = version;
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
    internal static SetupMachine Ready(ReleaseFixture bundle, FakeRegistrations registrations) => new(
        _ => true, () => true, new OperatingSystemRelease("ubuntu", "24.04"),
        _ => throw new InvalidOperationException("nothing should be asked on a ready machine"),
        (_, _) => throw new InvalidOperationException("nothing privileged should run on a ready machine"),
        _ => registrations, _ => Task.CompletedTask);
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
