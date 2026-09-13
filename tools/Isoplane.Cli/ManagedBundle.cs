using System.Formats.Tar;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
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
        var assets = Paths(items, "WispBundleAsset");
        if (assets.Length != 0)
            throw new InvalidOperationException("Managed deployment cannot yet carry external assets: " + string.Join(", ", assets.Select(Path.GetFileName)) + ". Embed application resources in the assembly or wait for deployment asset support.");
        var assemblies = ReadAssemblies(Paths(items, "WispBundleAssembly"));
        var platform = ReadAssemblies(Paths(items, "WispBundlePlatformReference"));
        var symbols = Paths(items, "WispBundleSymbol").ToHashSet(StringComparer.Ordinal);
        ValidateReferences(assemblies, platform);
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
                foreach (var assembly in assemblies.Values.OrderBy(value => value.Identity.Name, StringComparer.Ordinal))
                {
                    await tar.WriteEntryAsync(assembly.Path, "assemblies/" + assembly.Identity.Name + ".dll", cancellation);
                    var pdbPath = Path.ChangeExtension(assembly.Path, ".pdb");
                    if (symbols.Contains(pdbPath))
                    {
                        ValidateSymbols(pdbPath);
                        await tar.WriteEntryAsync(pdbPath, "assemblies/" + assembly.Identity.Name + ".pdb", cancellation);
                    }
                }
            }
            File.Move(partial, output, overwrite: true);
        }
        finally { File.Delete(partial); }
    }

    private sealed record AssemblyInput(string Path, AssemblyName Identity);

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

    private static Dictionary<string, AssemblyInput> ReadAssemblies(string[] paths)
    {
        var result = new Dictionary<string, AssemblyInput>(StringComparer.OrdinalIgnoreCase);
        foreach (var path in paths)
        {
            RequireRegularFile(path);
            AssemblyName identity;
            try { identity = AssemblyName.GetAssemblyName(path); }
            catch (BadImageFormatException error)
            {
                throw new InvalidOperationException($"Bundle input '{path}' is not a valid managed assembly.", error);
            }
            var name = identity.Name ?? throw new InvalidOperationException($"Assembly '{path}' has no identity.");
            if (!string.IsNullOrEmpty(identity.CultureName)) throw new InvalidOperationException($"Satellite assembly '{name}' requires deployment asset support.");
            if (name.IndexOfAny(['/', '\\']) >= 0) throw new InvalidOperationException($"Assembly '{path}' has an invalid identity.");
            if (!result.TryAdd(name, new(path, identity)) && result[name].Path != path)
                throw new InvalidOperationException($"Conflicting inputs define assembly '{name}': '{result[name].Path}' and '{path}'.");
        }
        return result;
    }

    private static void ValidateReferences(Dictionary<string, AssemblyInput> assemblies, Dictionary<string, AssemblyInput> platform)
    {
        foreach (var assembly in assemblies.Values)
        {
            if (platform.ContainsKey(assembly.Identity.Name!)) throw new InvalidOperationException($"Application input attempts to replace platform assembly '{assembly.Identity.Name}'.");
            using var file = File.OpenRead(assembly.Path);
            using var pe = new PEReader(file);
            if (pe.PEHeaders.CorHeader is not { } header || !header.Flags.HasFlag(CorFlags.ILOnly))
                throw new InvalidOperationException($"Application input '{assembly.Path}' must contain managed IL only.");
            var reader = pe.GetMetadataReader();
            foreach (var handle in reader.AssemblyReferences)
            {
                var reference = reader.GetAssemblyReference(handle);
                var name = reader.GetString(reference.Name);
                if (!assemblies.TryGetValue(name, out var resolved) && !platform.TryGetValue(name, out resolved))
                    throw new InvalidOperationException($"Assembly '{assembly.Identity.Name}' requires missing dependency '{name}' {reference.Version}.");
                var expected = new AssemblyName { Name = name, Version = reference.Version, CultureName = reader.GetString(reference.Culture) };
                if (reference.Flags.HasFlag(AssemblyFlags.PublicKey)) expected.SetPublicKey(reader.GetBlobBytes(reference.PublicKeyOrToken));
                else expected.SetPublicKeyToken(reader.GetBlobBytes(reference.PublicKeyOrToken));
                if (resolved.Identity.Version < expected.Version || !string.Equals(resolved.Identity.CultureName, expected.CultureName, StringComparison.OrdinalIgnoreCase) ||
                    !(resolved.Identity.GetPublicKeyToken() ?? []).SequenceEqual(expected.GetPublicKeyToken() ?? []))
                    throw new InvalidOperationException($"Assembly '{assembly.Identity.Name}' requires '{expected.FullName}', but the build resolved '{resolved.Identity.FullName}'.");
            }
        }
    }
}
