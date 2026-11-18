using System.Reflection;
using System.Runtime.Loader;
using System.Security.Cryptography;

namespace Isoplane.Debugging;

/// <summary>
/// Microsoft's Debug Adapter Protocol library. The tool package leaves it out; <c>isoplane setup</c> downloads it
/// into the installation once the user agrees to Microsoft's license terms, and the adapter loads it from there.
/// </summary>
public static class DebugProtocolLibrary
{
    /// <summary>The NuGet package identifier, which is also the assembly's simple name.</summary>
    public static string PackageId { get; } = Declared("DebugProtocolPackageId");

    /// <summary>The package version the debugger was compiled against.</summary>
    public static string PackageVersion { get; } = Declared("DebugProtocolPackageVersion");

    /// <summary>Lowercase hexadecimal SHA-256 of the package file nuget.org serves for <see cref="PackageVersion"/>.</summary>
    public static string PackageSha256 { get; } = Declared("DebugProtocolPackageSha256");

    /// <summary>The path inside the package of the one assembly the adapter loads.</summary>
    public static string AssemblyEntry { get; } = Declared("DebugProtocolAssemblyEntry");

    /// <summary>Lowercase hexadecimal SHA-256 of that assembly.</summary>
    public static string AssemblySha256 { get; } = Declared("DebugProtocolAssemblySha256");

    /// <summary>Where Microsoft publishes the license terms that govern the library.</summary>
    public static string LicenseUrl { get; } = Declared("DebugProtocolLicenseUrl");

    /// <summary>The assembly's location in the installation at <paramref name="home"/>, whether or not it is there.</summary>
    public static string PathIn(string home) => Path.Combine(home, Declared("DebugProtocolInstallDirectory"), PackageId + ".dll");


    /// <summary>
    /// Makes the assembly in the installation at <paramref name="home"/> the one this process loads.
    /// Fails with <see cref="DebugProtocolLibraryMissingException"/> when that installation lacks it or holds another version.
    /// </summary>
    internal static void Load(string home)
    {
        var path = PathIn(home);
        if (!File.Exists(path))
            throw new DebugProtocolLibraryMissingException(path, anotherVersion: false);
        using (var stream = File.OpenRead(path))
        {
            if (Convert.ToHexStringLower(SHA256.HashData(stream)) != AssemblySha256)
                throw new DebugProtocolLibraryMissingException(path, anotherVersion: true);
        }
        AssemblyLoadContext.Default.Resolving += (context, name) => name.Name == PackageId ? context.LoadFromAssemblyPath(path) : null;
    }

    private static string Declared(string key) =>
        typeof(DebugProtocolLibrary).Assembly.GetCustomAttributes<AssemblyMetadataAttribute>().Single(entry => entry.Key == key).Value
        ?? throw new InvalidOperationException($"Isoplane.Debugging was built without a value for {key}.");
}

/// <summary>Thrown when debugging starts in an installation that lacks Microsoft's Debug Adapter Protocol library.</summary>
public sealed class DebugProtocolLibraryMissingException : InvalidOperationException
{
    internal DebugProtocolLibraryMissingException(string path, bool anotherVersion) : base(
        $"Debugging needs Microsoft's {DebugProtocolLibrary.PackageId} {DebugProtocolLibrary.PackageVersion} library, and "
        + (anotherVersion ? $"{path} is another version of it" : $"it is not installed at {path}")
        + ". Run isoplane setup and agree to Microsoft's license terms to download it.")
    {
        Path = path;
    }

    /// <summary>Where the adapter looked for the assembly.</summary>
    public string Path { get; }
}
