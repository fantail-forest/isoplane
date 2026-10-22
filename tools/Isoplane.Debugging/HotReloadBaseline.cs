using System.Formats.Tar;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;

namespace Isoplane.Debugging;

/// <summary>Owns stable compiler inputs while development builds replace the application package.</summary>
internal sealed class HotReloadBaseline : IDisposable
{
    // The application owner can remove its session before the compiler releases its file handles.
    private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("isoplane-hotreload-");
    internal Dictionary<string, string> Assemblies { get; } = new(StringComparer.Ordinal);

    private HotReloadBaseline() { }

    internal static HotReloadBaseline Read(string package)
    {
        var baseline = new HotReloadBaseline();
        try
        {
            baseline.Extract(package);
            return baseline;
        }
        catch
        {
            baseline.Dispose();
            throw;
        }
    }

    private void Extract(string package)
    {
        var modules = new Dictionary<string, string>(StringComparer.Ordinal);
        using var file = new FileStream(package, FileMode.Open, FileAccess.Read, FileShare.Read | FileShare.Delete);
        using var archive = new TarReader(file);
        while (archive.GetNextEntry() is { } entry)
        {
            if (entry.DataStream is null || !entry.Name.StartsWith("assemblies/", StringComparison.Ordinal)) continue;
            string extension = Path.GetExtension(entry.Name);
            if (extension is not (".dll" or ".pdb")) continue;
            string name = entry.Name[..^extension.Length];
            if (!modules.TryGetValue(name, out string? destination))
            {
                destination = Path.Combine(directory.FullName, modules.Count.ToString(System.Globalization.CultureInfo.InvariantCulture));
                Directory.CreateDirectory(destination);
                modules.Add(name, destination);
            }
            using var output = new FileStream(Path.Combine(destination, "assembly" + extension), FileMode.CreateNew, FileAccess.Write);
            entry.DataStream.CopyTo(output);
        }
        foreach (string module in modules.Values)
        {
            string assembly = Path.Combine(module, "assembly.dll");
            if (!File.Exists(assembly)) throw new InvalidDataException("The debugger package contains symbols without their assembly.");
            using var image = File.OpenRead(assembly);
            using var pe = new PEReader(image);
            var metadata = pe.GetMetadataReader();
            var definition = metadata.GetAssemblyDefinition();
            if (!definition.Culture.IsNil && metadata.GetString(definition.Culture).Length != 0) continue;
            string name = metadata.GetString(definition.Name);
            if (!Assemblies.TryAdd(name, assembly)) throw new InvalidDataException($"The debugger package contains more than one baseline for '{name}'.");
        }
    }

    public void Dispose() => directory.Delete(recursive: true);
}
