using System.Formats.Tar;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class HotReloadBaselineTests
{
    [Fact]
    public void compiler_inputs_survive_package_replacement_and_session_shutdown_until_disposed()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-hotreload-baseline-");
        try
        {
            string package = Path.Combine(directory.FullName, "application.tar");
            string assembly = typeof(HotReloadBaseline).Assembly.Location;
            string symbols = Path.ChangeExtension(assembly, ".pdb");
            using (var file = File.Create(package))
            using (var archive = new TarWriter(file))
            {
                archive.WriteEntry(assembly, "assemblies/Isoplane.Debugging.dll");
                archive.WriteEntry(symbols, "assemblies/Isoplane.Debugging.pdb");
            }
            string snapshot;
            using (var baseline = HotReloadBaseline.Read(package))
            {
                snapshot = baseline.Assemblies["Isoplane.Debugging"];
                File.Delete(package);
                File.WriteAllText(package, "a subsequent build replaces this package");
                Assert.Equal(ModuleId(assembly), ModuleId(snapshot));
                Assert.Equal(File.ReadAllBytes(symbols), File.ReadAllBytes(Path.ChangeExtension(snapshot, ".pdb")));
                using var reader = File.OpenRead(snapshot);
                directory.Delete(recursive: true);
                Assert.Equal(ModuleId(assembly), ModuleId(snapshot));
            }
            Assert.False(File.Exists(snapshot));
            directory.Create();
            using (var file = File.Create(package))
            using (var archive = new TarWriter(file))
                archive.WriteEntry(symbols, "assemblies/missing.pdb");
            Assert.Throws<InvalidDataException>(() => HotReloadBaseline.Read(package));
            Assert.Equal([package], Directory.GetFileSystemEntries(directory.FullName));
        }
        finally { if (Directory.Exists(directory.FullName)) directory.Delete(recursive: true); }
    }

    private static Guid ModuleId(string path)
    {
        using var stream = File.OpenRead(path);
        using var pe = new PEReader(stream);
        var metadata = pe.GetMetadataReader();
        return metadata.GetGuid(metadata.GetModuleDefinition().Mvid);
    }
}
