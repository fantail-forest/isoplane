using Isoplane.Native;
using System.Diagnostics;
using System.Formats.Tar;
using System.IO.Compression;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json;

namespace Isoplane.Cli;

internal sealed record ToolchainFile(string Path, string Sha256, bool Executable);
internal sealed record ToolchainManifest(string RuntimeIdentifier, string DotnetSdkVersion, string WasiWorkloadVersion, ToolchainFile[] Files, string Kind = "toolchain");

internal static class Toolchain
{
    internal static string Home => Environment.GetEnvironmentVariable("ISOPLANE_HOME") is { Length: > 0 } configured
        ? Path.GetFullPath(configured)
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".isoplane");
    internal const string ManifestName = "toolchain.json";
    private static readonly JsonSerializerOptions Json = new() { PropertyNameCaseInsensitive = true, WriteIndented = true };

    internal static string Installations(string home) => Path.Combine(home, "toolchains");

    /// <summary>The installed toolchain with this identity when every file verifies, otherwise null.</summary>
    internal static async Task<string?> VerifiedInstallationAsync(string home, string identity, CancellationToken cancellation, string? bundledRuntime = null)
    {
        var destination = Path.Combine(Installations(home), identity);
        if (!File.Exists(Path.Combine(destination, ManifestName)))
            return null;
        try
        {
            var manifest = ReadManifest(await File.ReadAllBytesAsync(Path.Combine(destination, ManifestName), cancellation));
            ValidatePlatform(manifest);
            await VerifyAsync(destination, manifest, cancellation);
            if (bundledRuntime is not null)
                await VerifyRuntimePairAsync(manifest, bundledRuntime, cancellation);
            return destination;
        }
        catch (Exception error) when (error is IOException or InvalidOperationException)
        {
            Console.Error.WriteLine($"isoplane: replacing an incomplete or corrupt toolchain: {error.Message}");
            return null;
        }
    }

    /// <summary>
    /// Extracts a toolchain archive into a staging directory beside the installations, verifies every file against the
    /// manifest it carries and the identity the release promised, then moves it into place. A verification failure
    /// leaves no trace; an existing installation with the same identity is replaced only after the new one verified.
    /// </summary>
    internal static async Task<string> InstallArchiveAsync(string archive, string identity, string home, CancellationToken cancellation, string? bundledRuntime = null)
    {
        var installs = Installations(home);
        Directory.CreateDirectory(installs);
        var staging = Path.Combine(installs, ".install-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(staging);
        try
        {
            try
            {
                await using var stream = File.OpenRead(archive);
                await using var decompressed = new GZipStream(stream, CompressionMode.Decompress);
                await TarFile.ExtractToDirectoryAsync(decompressed, staging, overwriteFiles: false, cancellation);
            }
            catch (Exception error) when (error is IOException or InvalidDataException)
            {
                throw new InvalidOperationException($"The toolchain archive {archive} cannot be extracted: {error.Message}. The release is incomplete or altered; rerun isoplane setup to download it again.", error);
            }
            var manifestPath = Path.Combine(staging, ManifestName);
            if (!File.Exists(manifestPath))
                throw new InvalidOperationException($"The toolchain archive {archive} carries no {ManifestName}; the release is incomplete.");
            var manifestBytes = await File.ReadAllBytesAsync(manifestPath, cancellation);
            var actual = Convert.ToHexStringLower(SHA256.HashData(manifestBytes));
            if (actual != identity)
                throw new InvalidOperationException($"The toolchain archive {archive} is not the one the release manifest describes; the release is incomplete or altered.");
            var manifest = ReadManifest(manifestBytes);
            ValidatePlatform(manifest);
            if (!OperatingSystem.IsWindows())
                foreach (var entry in manifest.Files.Where(file => file.Executable))
                {
                    var path = Resolve(staging, entry.Path);
                    if (File.Exists(path))
                        File.SetUnixFileMode(path, File.GetUnixFileMode(path) | UnixFileMode.UserExecute);
                }
            await VerifyAsync(staging, manifest, cancellation);
            if (bundledRuntime is not null)
                await VerifyRuntimePairAsync(manifest, bundledRuntime, cancellation);
            var destination = Path.Combine(installs, identity);
            var backup = staging + ".previous";
            if (Directory.Exists(destination))
                Directory.Move(destination, backup);
            try
            {
                Directory.Move(staging, destination);
            }
            catch
            {
                if (Directory.Exists(backup))
                    Directory.Move(backup, destination);
                throw;
            }
            if (Directory.Exists(backup))
                Directory.Delete(backup, recursive: true);
            return destination;
        }
        finally
        {
            if (Directory.Exists(staging))
                Directory.Delete(staging, recursive: true);
        }
    }

    /// <summary>Records the workload-installed toolchain used by CLI child processes.</summary>
    internal static async Task SelectAsync(string home, string directory, CancellationToken cancellation)
    {
        await ReplaceAsync(Path.Combine(home, "current.json"), JsonSerializer.Serialize(directory), cancellation);
    }

    /// <summary>The selected toolchain directory, or null when none is selected.</summary>
    internal static string? Selected(string home)
    {
        var selected = Path.Combine(home, "current.json");
        return File.Exists(selected) ? JsonSerializer.Deserialize<string>(File.ReadAllText(selected)) : null;
    }

    internal static void Apply(ProcessStartInfo start, string? home = null)
    {
        if (!File.Exists(Path.Combine(home ?? Home, "current.json")))
            return;
        var directory = Selected(home ?? Home)
            ?? throw new InvalidOperationException("Invalid toolchain selection; rerun isoplane setup.");
        start.Environment["PATH"] = Path.Combine(directory, "bin") + Path.PathSeparator
            + (start.Environment.TryGetValue("PATH", out var searchPath) ? searchPath : "");
    }

    internal static async Task<string> DiagnoseAsync(string home, CancellationToken cancellation, string? bundledRuntime = null)
    {
        var current = Path.Combine(home, "current.json");
        if (!File.Exists(current))
            throw new InvalidOperationException("No toolchain is installed. Run isoplane setup.");
        var directory = JsonSerializer.Deserialize<string>(await File.ReadAllTextAsync(current, cancellation))
            ?? throw new InvalidOperationException("The toolchain selection is invalid; rerun isoplane setup.");
        var manifest = ReadManifest(await File.ReadAllBytesAsync(Path.Combine(directory, ManifestName), cancellation));
        ValidatePlatform(manifest);
        await VerifyAsync(directory, manifest, cancellation);
        if (bundledRuntime is not null)
            await VerifyRuntimePairAsync(manifest, bundledRuntime, cancellation);
        return directory;
    }

    internal static async Task VerifyAsync(string directory, ToolchainManifest manifest, CancellationToken cancellation)
    {
        foreach (var file in manifest.Files)
        {
            var path = Resolve(directory, file.Path);
            RejectLinks(directory, path);
            await using var stream = File.OpenRead(path);
            var actual = Convert.ToHexStringLower(await SHA256.HashDataAsync(stream, cancellation));
            if (!string.Equals(actual, file.Sha256, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException($"Toolchain file {path} is corrupt. Rerun isoplane setup to repair it.");
            if (!OperatingSystem.IsWindows() && file.Executable && (File.GetUnixFileMode(path) & UnixFileMode.UserExecute) == 0)
                throw new InvalidOperationException($"Toolchain file {path} is not executable; rerun setup from a complete bundle.");
        }
    }

    private static async Task VerifyRuntimePairAsync(ToolchainManifest manifest, string bundledRuntime, CancellationToken cancellation)
    {
        var filename = NativeRuntime.LibraryFilename;
        await using var stream = File.OpenRead(Path.Combine(bundledRuntime, filename));
        var digest = Convert.ToHexStringLower(await SHA256.HashDataAsync(stream, cancellation));
        var expected = manifest.Files.Single(file => file.Path == "cli/runtime/" + filename).Sha256;
        if (!string.Equals(digest, expected, StringComparison.OrdinalIgnoreCase))
            throw new InvalidOperationException("The CLI and toolchain contain different native libraries. Install both from the same preview release.");
    }

    internal static ToolchainManifest ReadManifest(byte[] bytes)
    {
        var manifest = JsonSerializer.Deserialize<ToolchainManifest>(bytes, Json)
            ?? throw new InvalidOperationException("The toolchain bundle has no manifest.");
        if (string.IsNullOrWhiteSpace(manifest.RuntimeIdentifier) || string.IsNullOrWhiteSpace(manifest.DotnetSdkVersion) || string.IsNullOrWhiteSpace(manifest.WasiWorkloadVersion) || manifest.Files is not { Length: > 0 })
            throw new InvalidOperationException("The toolchain manifest must identify its SDK and files.");
        if (manifest.Kind is not ("toolchain" or "workload" or "msi"))
            throw new InvalidOperationException($"Unknown toolchain bundle kind: {manifest.Kind}.");
        var names = new HashSet<string>(StringComparer.Ordinal);
        foreach (var file in manifest.Files)
        {
            if (file is null || string.IsNullOrWhiteSpace(file.Path) || string.IsNullOrWhiteSpace(file.Sha256)
                || !names.Add(file.Path) || file.Sha256.Length != 64 || !file.Sha256.All(Uri.IsHexDigit))
                throw new InvalidOperationException($"Invalid or duplicate toolchain manifest entry: {file?.Path ?? "<null>"}.");
            Resolve(Path.GetTempPath(), file.Path);
        }
        foreach (var required in manifest.Kind == "workload"
            ? new[] { "WorkloadManifest.json", "WorkloadManifest.targets" }
            : manifest.Kind == "msi" ? new[] { "sdk.msi", "WindowsInstaller.json", "vc_redist.x64.exe" } : RequiredFiles())
            if (!names.Contains(required))
                throw new InvalidOperationException($"The toolchain manifest is incomplete: missing {required}.");
        return manifest;
    }

    internal static string ExecutableExtension => OperatingSystem.IsWindows() ? ".exe" : "";

    internal static string[] RequiredFiles() => [
        "cli/Isoplane.Cli.dll", "cli/Isoplane.Cli.runtimeconfig.json", "cli/Isoplane.Cli.deps.json",
        "cli/Isoplane.Native.dll", "cli/runtime/" + NativeRuntime.LibraryFilename, "bin/wasm-tools" + ExecutableExtension, "wasi-sdk/bin/clang" + ExecutableExtension,
        "feed/Isoplane.GuestRuntime.props"
    ];

    internal static string PlatformIdentifier()
    {
        var os = OperatingSystem.IsLinux()
            ? (RuntimeInformation.RuntimeIdentifier.Contains("musl", StringComparison.Ordinal) ? "linux-musl" : "linux")
            : OperatingSystem.IsWindows() ? "win" : OperatingSystem.IsMacOS() ? "osx" : "unsupported";
        var rid = os + "-" + RuntimeInformation.ProcessArchitecture.ToString().ToLowerInvariant();
        return rid;
    }

    private static void ValidatePlatform(ToolchainManifest manifest)
    {
        var rid = PlatformIdentifier();
        if (rid != manifest.RuntimeIdentifier)
            throw new InvalidOperationException($"Toolchain targets {manifest.RuntimeIdentifier}; this process runs on {rid}.");
    }

    internal static string Resolve(string directory, string relative)
    {
        if (string.IsNullOrWhiteSpace(relative) || Path.IsPathRooted(relative) || relative.Split('/', '\\').Any(part => part is ".." or "." or ""))
            throw new InvalidOperationException($"Unsafe toolchain path: {relative}.");
        return Path.Combine(directory, relative);
    }

    private static void RejectLinks(string root, string path)
    {
        for (var current = path; current != root; current = Path.GetDirectoryName(current)!)
            if ((File.GetAttributes(current) & FileAttributes.ReparsePoint) != 0)
                throw new InvalidOperationException($"Toolchain entry must not be a symbolic link: {current}.");
    }

    private static async Task ReplaceAsync(string path, string content, CancellationToken cancellation)
    {
        var temporary = path + "." + Guid.NewGuid().ToString("N");
        try
        {
            await File.WriteAllTextAsync(temporary, content, cancellation);
            File.Move(temporary, path, overwrite: true);
        }
        finally
        {
            if (File.Exists(temporary))
                File.Delete(temporary);
        }
    }
}
