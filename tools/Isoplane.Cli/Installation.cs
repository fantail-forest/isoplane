using System.Text.Json;
using System.Text.Json.Serialization;

namespace Isoplane.Cli;

internal enum InstalledResourceKind
{
    /// <summary>A directory setup created, removed recursively.</summary>
    Directory,
    File,
    /// <summary>A link recorded by a pre-workload installation; retained for removal.</summary>
    SymbolicLink,
    /// <summary>A NuGet package source in the user's NuGet configuration; the target is its name, the detail its path.</summary>
    PackageSource,
    /// <summary>A per-user template registration maintained by dotnet new.</summary>
    Template,
    /// <summary>A shell entry from a pre-workload installation; the detail is the exact line to remove.</summary>
    ProfileLine,
    /// <summary>A workload installation marker; the detail identifies its owning .NET SDK.</summary>
    Workload,
    /// <summary>A manifest version directory; the detail identifies its owning .NET SDK.</summary>
    WorkloadManifest,
    /// <summary>A Windows Installer product owned by setup.</summary>
    WindowsInstaller,
}

/// <summary>
/// A resource recorded by setup, with the ownership used to decide whether removal deletes it.
/// </summary>
internal sealed record InstalledResource(InstalledResourceKind Kind, string Target, bool Owned, string? Detail = null, string? RemovalFailure = null);

internal sealed record InstallationRecord(int Schema, string CliVersion, string SourceRevision, string Home, List<InstalledResource> Resources)
{
    internal const int CurrentSchema = 1;
}

/// <summary>Thrown when the installation manifest exists but cannot be trusted; removal must not guess in its place.</summary>
internal sealed class InstallationManifestException(string message, Exception? inner = null) : InvalidOperationException(message, inner);

/// <summary>
/// Reads and persists setup's resource records at <c>ISOPLANE_HOME/installation.json</c>.
/// </summary>
internal sealed class Installation
{
    internal const string FileName = "installation.json";
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true,
        Converters = { new JsonStringEnumConverter() },
        UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow,
    };

    private readonly string home;
    private InstallationRecord record;

    private Installation(string home, InstallationRecord record)
    {
        this.home = home;
        this.record = record;
    }

    internal static string PathIn(string home) => System.IO.Path.Combine(home, FileName);

    internal string Path => PathIn(home);
    internal IReadOnlyList<InstalledResource> Resources => record.Resources;
    internal string CliVersion => record.CliVersion;

    /// <summary>Loads the manifest, or null when none exists. A damaged manifest is an error naming the file.</summary>
    internal static Installation? Load(string home)
    {
        var path = PathIn(home);
        if (!File.Exists(path))
            return null;
        InstallationRecord? record;
        try
        {
            record = JsonSerializer.Deserialize<InstallationRecord>(File.ReadAllBytes(path), Json);
        }
        catch (JsonException error)
        {
            throw new InstallationManifestException($"The installation manifest at {path} is damaged ({error.Message}). Nothing was removed. Restore it, or if the SDK is already gone, delete the file and run isoplane setup again.", error);
        }
        if (record is null || record.Schema != InstallationRecord.CurrentSchema || record.Resources is null
            || string.IsNullOrWhiteSpace(record.Home) || record.Resources.Any(resource => resource is null || string.IsNullOrWhiteSpace(resource.Target)))
            throw new InstallationManifestException($"The installation manifest at {path} is not one this CLI can read. Nothing was removed. Restore it, or if the SDK is already gone, delete the file and run isoplane setup again.");
        return new(home, record);
    }

    /// <summary>The existing manifest, or a new one for this CLI. An existing manifest is re-stamped with the CLI now installing.</summary>
    internal static Installation Begin(string home, ReleaseIdentity cli)
    {
        var existing = Load(home);
        var record = existing?.record is { } previous
            ? previous with { CliVersion = cli.Version, SourceRevision = cli.SourceRevision }
            : new InstallationRecord(InstallationRecord.CurrentSchema, cli.Version, cli.SourceRevision, home, []);
        var installation = new Installation(home, record);
        installation.Save();
        return installation;
    }

    /// <summary>Records a resource before or as it is created, replacing an earlier record of the same kind and target.</summary>
    internal InstalledResource Record(InstalledResource resource)
    {
        var index = record.Resources.FindIndex(existing => Same(existing, resource));
        if (index < 0)
            record.Resources.Add(resource);
        else
            record.Resources[index] = resource;
        Save();
        return resource;
    }

    internal InstalledResource? Find(InstalledResourceKind kind, string target) =>
        record.Resources.FirstOrDefault(resource => resource.Kind == kind && resource.Target == target);

    /// <summary>Drops a resource that no longer exists.</summary>
    internal void Forget(InstalledResource resource)
    {
        record.Resources.RemoveAll(existing => Same(existing, resource));
        Save();
    }

    /// <summary>Deletes the manifest once nothing it recorded remains.</summary>
    internal void Delete() => File.Delete(Path);

    private static bool Same(InstalledResource left, InstalledResource right) => left.Kind == right.Kind && left.Target == right.Target;

    private void Save()
    {
        Directory.CreateDirectory(home);
        var temporary = Path + "." + Guid.NewGuid().ToString("N");
        try
        {
            File.WriteAllBytes(temporary, JsonSerializer.SerializeToUtf8Bytes(record, Json));
            File.Move(temporary, Path, overwrite: true);
        }
        finally
        {
            if (File.Exists(temporary))
                File.Delete(temporary);
        }
    }
}
