using System.Runtime.InteropServices;

namespace Isoplane.Cli;

/// <summary>A native library the installed toolchain loads, with the Ubuntu 24.04 package that provides it.</summary>
internal sealed record NativePrerequisite(string Name, string[] Sonames, string Purpose, string UbuntuPackage);

internal sealed record OperatingSystemRelease(string Id, string VersionId)
{
    internal bool IsSupportedUbuntu => Id == "ubuntu" && VersionId == "24.04";
}

/// <summary>What the machine must already provide for the toolchain to run: the loader must find these libraries.</summary>
internal static class Prerequisites
{
    internal const string CertificateBundle = "/etc/ssl/certs/ca-certificates.crt";

    internal static readonly NativePrerequisite[] Libraries =
    [
        new("ICU", Enumerable.Range(60, 41).Reverse().Select(version => $"libicuuc.so.{version}").ToArray(), "globalization in the .NET SDK", "libicu74"),
        new("OpenSSL", ["libssl.so.3"], "TLS in the .NET SDK", "libssl3t64"),
        new("zlib", ["libz.so.1"], "the NativeAOT compiler backend", "zlib1g"),
        new("libstdc++", ["libstdc++.so.6"], "the .NET SDK and the WebAssembly compiler", "libstdc++6"),
        new("zstd", ["libzstd.so.1"], "the NativeAOT compiler backend", "libzstd1"),
        new("terminfo", ["libtinfo.so.6"], "the NativeAOT compiler backend", "libtinfo6"),
        new("CA certificates", [], "downloads and TLS connections", "ca-certificates"),
    ];

    /// <summary>The prerequisites the loader cannot satisfy right now.</summary>
    internal static IReadOnlyList<NativePrerequisite> Missing(Func<string, bool> canLoad, Func<string, bool> fileExists) =>
        Libraries.Where(library => library.Sonames.Length == 0 ? !fileExists(CertificateBundle) : !library.Sonames.Any(canLoad)).ToArray();

    internal static bool CanLoad(string soname)
    {
        if (!NativeLibrary.TryLoad(soname, out var handle))
            return false;
        NativeLibrary.Free(handle);
        return true;
    }

    /// <summary>Reads <c>/etc/os-release</c>; an unreadable file is an unknown distribution.</summary>
    internal static OperatingSystemRelease Detect(string path = "/etc/os-release")
    {
        if (!File.Exists(path))
            return new("unknown", "");
        var fields = File.ReadLines(path)
            .Select(line => line.Split('=', 2))
            .Where(parts => parts.Length == 2)
            .ToDictionary(parts => parts[0].Trim(), parts => parts[1].Trim().Trim('"'), StringComparer.Ordinal);
        return new(fields.GetValueOrDefault("ID", "unknown"), fields.GetValueOrDefault("VERSION_ID", ""));
    }

    /// <summary>The one command that installs the missing prerequisites, when this distribution is supported.</summary>
    internal static string? InstallCommand(OperatingSystemRelease release, IEnumerable<NativePrerequisite> missing) =>
        release.IsSupportedUbuntu ? "sudo apt-get install -y " + string.Join(' ', missing.Select(library => library.UbuntuPackage)) : null;

    internal static string Describe(IEnumerable<NativePrerequisite> missing) =>
        string.Join(", ", missing.Select(library => library.Sonames.Length == 0 ? $"{library.Name} ({CertificateBundle}, for {library.Purpose})" : $"{library.Name} ({library.Sonames[^1]}, for {library.Purpose})"));
}
