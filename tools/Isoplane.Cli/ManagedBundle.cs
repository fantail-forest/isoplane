using System.Formats.Tar;
using System.Reflection.Metadata;
using System.Text.Json;
using Isoplane.Build;

namespace Isoplane.Cli;

internal sealed record BundleOptions(string Directory, string Configuration, string Output, bool NoBuild = false)
{
    internal static BundleOptions Parse(string[] arguments)
    {
        var directory = Environment.CurrentDirectory;
        var configuration = "Release";
        string? output = null;
        for (var index = 0; index < arguments.Length; index++)
        {
            var option = arguments[index];
            if (++index == arguments.Length) throw new ArgumentException($"{option} requires a value.");
            switch (option)
            {
                case "--project": directory = Path.GetFullPath(arguments[index]); break;
                case "--configuration": configuration = arguments[index]; break;
                case "--output": output = Path.GetFullPath(arguments[index]); break;
                default: throw new ArgumentException($"Unknown bundle option '{option}'.");
            }
        }
        if (string.IsNullOrWhiteSpace(configuration)) throw new ArgumentException("The build configuration must not be empty.");
        return new(directory, configuration, output ?? throw new ArgumentException("bundle requires --output FILE."));
    }
}

/// <summary>The runtimes an SDK carries for interpreted applications, in <paramref name="Directory"/>.</summary>
internal sealed record SharedRuntime(string Directory)
{
    internal string Module => Require("Isoplane.MonoRuntime.wasm");

    /// <summary>The runtime that connects its debugger agent to a local debug session.</summary>
    internal string DebugModule => Require("Isoplane.MonoRuntime.Debug.wasm");

    /// <summary>The runtimes' identity, which a package built against them carries so the platform runs it on the same runtime.</summary>
    internal string Identity => Require("runtime.json");

    private string Require(string name)
    {
        var path = Path.Combine(Directory, name);
        return File.Exists(path) ? path
            : throw new InvalidOperationException($"The referenced Isoplane.Sdk has no shared runtime at '{path}'; restore an SDK release that includes its shared runtime.");
    }
}

internal static class ManagedBundle
{
    /// <summary>
    /// Builds the project's package into <see cref="BundleOptions.Output"/> and
    /// returns the shared runtimes its referenced SDK carries.
    /// </summary>
    internal static async Task<SharedRuntime> CreateAsync(BundleOptions options, CancellationToken cancellation)
    {
        string project = ProjectFile(options.Directory);
        var temporary = Directory.CreateDirectory(new LocalProject(options.Directory).Scratch("bundle"));
        try
        {
            var metadata = Path.Combine(temporary.FullName, "inputs.json");
            List<string> arguments = ["msbuild", project, "-target:IsoplaneManagedBundle",
                "-nodeReuse:false", "-property:UseSharedCompilation=false",
                "-property:IsoplaneManagedBundleBuild=true", "-property:Configuration=" + options.Configuration,
                "-getProperty:AssemblyName,IsoplaneRuntimeDirectory",
                "-getItem:IsoplaneBundleAssembly,IsoplaneBundleSymbol,IsoplaneBundleAsset,IsoplaneBundlePlatformReference",
                "-getResultOutputFile:" + metadata, "-nologo"];
            if (options.NoBuild)
                arguments.AddRange(["-property:NoBuild=true", "-property:BuildProjectReferences=false"]);
            else
                arguments.Add("-restore");
            var exit = await Processes.RunAsync("dotnet", arguments.ToArray(), options.Directory, cancellation, redirectOutput: true);
            if (exit != 0) throw new InvalidOperationException($"Managed application compilation failed (exit {exit}); resolve the build diagnostics before submitting.");
            using var document = JsonDocument.Parse(await File.ReadAllBytesAsync(metadata, cancellation));
            await WriteAsync(document.RootElement, options.Output, SourceDate.FromEnvironment(), cancellation);
            return new(document.RootElement.GetProperty("Properties").GetProperty("IsoplaneRuntimeDirectory").GetString() ?? "");
        }
        finally { temporary.Delete(recursive: true); }
    }

    internal static string ProjectFile(string directory)
    {
        var projects = Directory.GetFiles(directory, "*.csproj");
        if (projects.Length != 1) throw new ArgumentException("Select a directory containing exactly one application project with --project.");
        return Path.GetFullPath(projects[0]);
    }

    /// <summary>
    /// Writes the package. Each file is dated as its source is, or as
    /// <paramref name="sourceDate"/> when one is given, and carries no owner or
    /// permissions of the machine it was built on.
    /// </summary>
    internal static async Task WriteAsync(JsonElement metadata, string output, DateTimeOffset? sourceDate, CancellationToken cancellation)
    {
        var properties = metadata.GetProperty("Properties");
        var items = metadata.GetProperty("Items");
        var assets = ReadAssets(items);
        var assemblies = ReadAssemblies(Paths(items, "IsoplaneBundleAssembly"));
        var platform = ReadAssemblies(Paths(items, "IsoplaneBundlePlatformReference"), platform: true);
        var symbols = Paths(items, "IsoplaneBundleSymbol").ToHashSet(StringComparer.Ordinal);
        AssemblyClosure.Validate(assemblies, platform);
        var entry = properties.GetProperty("AssemblyName").GetString();
        if (!assemblies.ContainsKey(entry ?? "")) throw new InvalidOperationException("The bundle has no application entry assembly.");
        var application = ApplicationEntryDiscovery.Discover(assemblies[entry!].Origin);
        var manifest = JsonSerializer.SerializeToUtf8Bytes(new ManagedApplication(entry!, application, GuestExecution.Interpreted), BuildJson.Options);
        var runtime = new SharedRuntime(properties.GetProperty("IsoplaneRuntimeDirectory").GetString() ?? "").Identity;
        var partial = output + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            await using (var file = new FileStream(partial, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            await using (var tar = new TarWriter(file, leaveOpen: true))
            {
                // An application sees each entry's time as that file's own. The manifest is read
                // out of the entry assembly, so it is as old as that assembly.
                await WriteEntryAsync(tar, "application.json", manifest,
                    sourceDate ?? File.GetLastWriteTimeUtc(assemblies[entry!].Origin), cancellation);
                await WriteFileAsync(tar, "runtime.json", runtime, sourceDate, cancellation);
                foreach (var assembly in assemblies.Values.OrderBy(value => value.Identity.Key, StringComparer.Ordinal))
                {
                    await WriteFileAsync(tar, "assemblies/" + assembly.Identity.RelativePath, assembly.Origin, sourceDate, cancellation);
                    var pdbPath = Path.ChangeExtension(assembly.Origin, ".pdb");
                    if (symbols.Contains(pdbPath))
                    {
                        ValidateSymbols(pdbPath);
                        await WriteFileAsync(tar, "assemblies/" + Path.ChangeExtension(assembly.Identity.RelativePath, ".pdb"), pdbPath, sourceDate, cancellation);
                    }
                }
                foreach (var asset in assets)
                {
                    if (sourceDate is { } date && asset.Target.EndsWith(StaticAssetEndpoints.Suffix, StringComparison.Ordinal))
                        await WriteEntryAsync(tar, "assets/" + asset.Target,
                            StaticAssetEndpoints.Dated(await File.ReadAllBytesAsync(asset.Source, cancellation), date), date, cancellation);
                    else
                        await WriteFileAsync(tar, "assets/" + asset.Target, asset.Source, sourceDate, cancellation);
                }
            }
            File.Move(partial, output, overwrite: true);
        }
        finally { File.Delete(partial); }
    }

    private static async Task WriteFileAsync(TarWriter tar, string name, string source, DateTimeOffset? sourceDate, CancellationToken cancellation)
    {
        await using var content = File.OpenRead(source);
        await WriteEntryAsync(tar, name, content, sourceDate ?? File.GetLastWriteTimeUtc(source), cancellation);
    }

    private static async Task WriteEntryAsync(TarWriter tar, string name, byte[] content, DateTimeOffset modified, CancellationToken cancellation)
    {
        using var stream = new MemoryStream(content, writable: false);
        await WriteEntryAsync(tar, name, stream, modified, cancellation);
    }

    // The entry is described here and not read from the file's own metadata, which would put
    // the builder's user name, identifiers and file permissions into the package.
    private static Task WriteEntryAsync(TarWriter tar, string name, Stream content, DateTimeOffset modified, CancellationToken cancellation) =>
        tar.WriteEntryAsync(new PaxTarEntry(TarEntryType.RegularFile, name) { DataStream = content, ModificationTime = modified }, cancellation);

    private static (string Source, string Target)[] ReadAssets(JsonElement items)
    {
        var result = new List<(string Source, string Target)>();
        var names = new HashSet<string>(StringComparer.Ordinal);
        foreach (var item in items.GetProperty("IsoplaneBundleAsset").EnumerateArray())
        {
            var source = Path.GetFullPath(item.GetProperty("FullPath").GetString() ?? throw new InvalidOperationException("The SDK returned an empty asset source."));
            RequireRegularFile(source);
            var target = item.TryGetProperty("TargetPath", out var targetPath) ? targetPath.GetString() : null;
            if (string.IsNullOrEmpty(target)) target = item.TryGetProperty("DestinationSubPath", out var destination) ? destination.GetString() : null;
            if (string.IsNullOrEmpty(target))
            {
                var subdirectory = item.TryGetProperty("DestinationSubDirectory", out var sub) ? sub.GetString() : "";
                target = subdirectory + Path.GetFileName(source);
            }
            target = target.Replace('\\', '/');
            if (!AssetPaths.TryAdd(names, target))
                throw new InvalidOperationException($"Bundle asset '{target}' is not a unique relative file path.");
            result.Add((source, target));
        }
        return result.OrderBy(asset => asset.Target, StringComparer.Ordinal).ToArray();
    }

    private static void ValidateSymbols(string path)
    {
        try
        {
            RequireRegularFile(path);
            using var pdb = File.OpenRead(path);
            using var provider = MetadataReaderProvider.FromPortablePdbStream(pdb);
            _ = provider.GetMetadataReader();
        }
        catch (BadImageFormatException error)
        {
            throw new InvalidOperationException($"Symbols '{path}' must be a portable PDB.", error);
        }
    }

    private static void RequireRegularFile(string path)
    {
        if ((File.GetAttributes(path) & (FileAttributes.ReparsePoint | FileAttributes.Directory)) != 0)
            throw new InvalidOperationException($"Bundle input '{path}' must be a regular file, not a link or directory.");
    }

    private static string[] Paths(JsonElement items, string name) => items.GetProperty(name).EnumerateArray()
        .Select(item => Path.GetFullPath(item.GetProperty("FullPath").GetString() ?? throw new InvalidOperationException("The SDK returned an empty build input path.")))
        .Distinct(StringComparer.Ordinal).ToArray();

    private static IReadOnlyDictionary<string, InspectedAssembly> ReadAssemblies(string[] paths, bool platform = false)
    {
        foreach (var path in paths) RequireRegularFile(path);
        var metadata = paths.Select(AssemblyClosure.InspectFile);
        return platform ? AssemblyClosure.Platform(metadata) : AssemblyClosure.Application(metadata);
    }
}

/// <summary>
/// The date a reproducible build gives everything it produces, from the
/// <c>SOURCE_DATE_EPOCH</c> environment variable: seconds since 1970, by
/// convention the time of the commit being built.
/// </summary>
internal static class SourceDate
{
    internal const string Variable = "SOURCE_DATE_EPOCH";

    internal static DateTimeOffset? FromEnvironment() => Parse(Environment.GetEnvironmentVariable(Variable));

    internal static DateTimeOffset? Parse(string? value)
    {
        if (string.IsNullOrEmpty(value)) return null;
        if (!long.TryParse(value, System.Globalization.NumberStyles.None, System.Globalization.CultureInfo.InvariantCulture, out var seconds))
            throw new InvalidOperationException($"{Variable} is '{value}'; set it to a whole number of seconds since 1970-01-01T00:00:00Z, or unset it.");
        try { return DateTimeOffset.FromUnixTimeSeconds(seconds); }
        catch (ArgumentOutOfRangeException)
        {
            throw new InvalidOperationException($"{Variable} is '{value}', which is later than any date .NET represents; set it to the time of the commit being built.");
        }
    }
}

/// <summary>
/// The manifest the .NET SDK generates for the files in <c>wwwroot</c>. It
/// records each file's modification time as the <c>Last-Modified</c> header
/// ASP.NET Core sends, so the manifest's own bytes depend on when the files
/// were written.
/// </summary>
internal static class StaticAssetEndpoints
{
    internal const string Suffix = ".staticwebassets.endpoints.json";

    /// <summary>The manifest with every file dated <paramref name="date"/>.</summary>
    internal static byte[] Dated(byte[] manifest, DateTimeOffset date)
    {
        var document = System.Text.Json.Nodes.JsonNode.Parse(manifest)
            ?? throw new InvalidOperationException("The static web assets manifest is empty.");
        foreach (var endpoint in document["Endpoints"]?.AsArray() ?? [])
            foreach (var header in endpoint?["ResponseHeaders"]?.AsArray() ?? [])
                if (header?["Name"]?.GetValue<string>() == "Last-Modified")
                    header["Value"] = date.ToString("R", System.Globalization.CultureInfo.InvariantCulture);
        return System.Text.Encoding.UTF8.GetBytes(document.ToJsonString());
    }
}
