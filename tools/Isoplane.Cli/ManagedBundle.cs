using System.Formats.Tar;
using System.Reflection.Metadata;
using System.Text.Json;
using Wisp.Build;

namespace Wisp.Cli;

internal sealed record BundleOptions(string Directory, string Configuration, string Output)
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

internal static class ManagedBundle
{
    internal static async Task CreateAsync(BundleOptions options, string runtime, CancellationToken cancellation)
    {
        var projects = Directory.GetFiles(options.Directory, "*.csproj");
        if (projects.Length != 1) throw new ArgumentException("Select a directory containing exactly one application project with --project.");
        var temporary = Directory.CreateTempSubdirectory("isoplane-bundle-");
        try
        {
            var metadata = Path.Combine(temporary.FullName, "inputs.json");
            var exit = await Processes.RunAsync("dotnet", ["msbuild", projects[0], "-restore", "-target:WispManagedBundle",
                "-nodeReuse:false", "-property:UseSharedCompilation=false",
                "-property:WispManagedBundleBuild=true", "-property:Configuration=" + options.Configuration,
                "-getProperty:AssemblyName,WispApplicationFactory,WispGuestRuntime",
                "-getItem:WispBundleAssembly,WispBundleSymbol,WispBundleAsset,WispBundlePlatformReference",
                "-getResultOutputFile:" + metadata, "-nologo"], options.Directory, runtime, cancellation, redirectOutput: true);
            if (exit != 0) throw new InvalidOperationException($"Managed application compilation failed (exit {exit}); resolve the build diagnostics before submitting.");
            using var document = JsonDocument.Parse(await File.ReadAllBytesAsync(metadata, cancellation));
            await WriteAsync(document.RootElement, options.Output, cancellation);
        }
        finally { temporary.Delete(recursive: true); }
    }

    internal static async Task WriteAsync(JsonElement metadata, string output, CancellationToken cancellation)
    {
        var properties = metadata.GetProperty("Properties");
        var items = metadata.GetProperty("Items");
        var assets = ReadAssets(items);
        var assemblies = ReadAssemblies(Paths(items, "WispBundleAssembly"));
        var platform = ReadAssemblies(Paths(items, "WispBundlePlatformReference"), platform: true);
        var symbols = Paths(items, "WispBundleSymbol").ToHashSet(StringComparer.Ordinal);
        AssemblyClosure.Validate(assemblies, platform);
        var entry = properties.GetProperty("AssemblyName").GetString();
        if (!assemblies.ContainsKey(entry ?? "")) throw new InvalidOperationException("The bundle has no application entry assembly.");
        var execution = properties.GetProperty("WispGuestRuntime").GetString() switch
        {
            "Mono" => GuestExecution.Interpreted,
            "NativeAOT" => GuestExecution.NativeAot,
            _ => throw new InvalidOperationException("The project must select an interpreted or NativeAOT configuration.")
        };
        var factory = properties.GetProperty("WispApplicationFactory").GetString();
        if (string.IsNullOrWhiteSpace(factory)) throw new InvalidOperationException("The project must name its application factory.");
        var manifest = JsonSerializer.SerializeToUtf8Bytes(new ManagedApplication(entry!, factory, execution), BuildJson.Options);
        var partial = output + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            await using (var file = new FileStream(partial, FileMode.CreateNew, FileAccess.Write, FileShare.None))
            await using (var tar = new TarWriter(file, leaveOpen: true))
            {
                using var content = new MemoryStream(manifest, writable: false);
                await tar.WriteEntryAsync(new PaxTarEntry(TarEntryType.RegularFile, "application.json") { DataStream = content }, cancellation);
                foreach (var assembly in assemblies.Values.OrderBy(value => value.Identity.Key, StringComparer.Ordinal))
                {
                    await tar.WriteEntryAsync(assembly.Origin, "assemblies/" + assembly.Identity.RelativePath, cancellation);
                    var pdbPath = Path.ChangeExtension(assembly.Origin, ".pdb");
                    if (symbols.Contains(pdbPath))
                    {
                        ValidateSymbols(pdbPath);
                        await tar.WriteEntryAsync(pdbPath, "assemblies/" + Path.ChangeExtension(assembly.Identity.RelativePath, ".pdb"), cancellation);
                    }
                }
                foreach (var asset in assets)
                    await tar.WriteEntryAsync(asset.Source, "assets/" + asset.Target, cancellation);
            }
            File.Move(partial, output, overwrite: true);
        }
        finally { File.Delete(partial); }
    }

    private static (string Source, string Target)[] ReadAssets(JsonElement items)
    {
        var result = new List<(string Source, string Target)>();
        var names = new HashSet<string>(StringComparer.Ordinal);
        foreach (var item in items.GetProperty("WispBundleAsset").EnumerateArray())
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
            target = target.Replace(Path.DirectorySeparatorChar, '/');
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
