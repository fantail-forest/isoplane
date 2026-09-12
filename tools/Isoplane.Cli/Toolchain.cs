using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text.Json;
using System.Xml.Linq;

namespace Wisp.Cli;

internal sealed record ToolchainFile(string Path, string Sha256, bool Executable);
internal sealed record ToolchainManifest(string RuntimeIdentifier, string DotnetSdkVersion, string WasiWorkloadVersion, ToolchainFile[] Files);

internal static class Toolchain
{
    internal static string Home => Environment.GetEnvironmentVariable("ISOPLANE_HOME") is { Length: > 0 } configured
        ? Path.GetFullPath(configured)
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".isoplane");
    private const string ManifestName = "toolchain.json";
    private static readonly JsonSerializerOptions Json = new() { PropertyNameCaseInsensitive = true, WriteIndented = true };

    internal static async Task<string> InstallAsync(string source, string home, CancellationToken cancellation, string? bundledRuntime = null)
    {
        source = Path.GetFullPath(source);
        home = Path.GetFullPath(home);
        var manifestBytes = await File.ReadAllBytesAsync(Path.Combine(source, ManifestName), cancellation);
        var manifest = ReadManifest(manifestBytes);
        ValidatePlatform(manifest);
        if (bundledRuntime is not null)
            await VerifyRuntimePairAsync(manifest, bundledRuntime, cancellation);
        var identity = Convert.ToHexStringLower(SHA256.HashData(manifestBytes));
        var installs = Path.Combine(home, "toolchains");
        Directory.CreateDirectory(installs);
        using var installLock = new FileStream(Path.Combine(home, "setup.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        var destination = Path.Combine(installs, identity);
        var reusable = false;
        if (Directory.Exists(destination))
        {
            try
            {
                await VerifyAsync(destination, manifest, cancellation);
                reusable = true;
            }
            catch (Exception error) when (error is IOException or InvalidOperationException)
            {
                Console.Error.WriteLine($"isoplane: repairing incomplete or corrupt toolchain: {error.Message}");
            }
        }
        if (!reusable)
        {
            var staging = Path.Combine(installs, ".install-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(staging);
            try
            {
                foreach (var entry in manifest.Files)
                {
                    cancellation.ThrowIfCancellationRequested();
                    var input = Resolve(source, entry.Path);
                    RejectLinks(source, input);
                    var output = Resolve(staging, entry.Path);
                    Directory.CreateDirectory(Path.GetDirectoryName(output)!);
                    File.Copy(input, output);
                    if (!OperatingSystem.IsWindows() && entry.Executable)
                        File.SetUnixFileMode(output, File.GetUnixFileMode(output) | UnixFileMode.UserExecute);
                }
                await File.WriteAllBytesAsync(Path.Combine(staging, ManifestName), manifestBytes, cancellation);
                await VerifyAsync(staging, manifest, cancellation);
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
            }
            finally
            {
                if (Directory.Exists(staging))
                    Directory.Delete(staging, recursive: true);
            }
        }
        SelectDotnet(home, Path.Combine(destination, "dotnet"));
        await ReplaceAsync(Path.Combine(home, "toolchain.props"), Properties(destination), cancellation);
        await ReplaceAsync(Path.Combine(home, "current.json"), JsonSerializer.Serialize(destination), cancellation);
        return destination;
    }

    internal static void Apply(ProcessStartInfo start, string? home = null)
    {
        var selected = Path.Combine(home ?? Home, "current.json");
        if (!File.Exists(selected))
            return;
        var directory = JsonSerializer.Deserialize<string>(File.ReadAllText(selected))
            ?? throw new InvalidOperationException("Invalid toolchain selection; rerun isoplane setup.");
        if (start.FileName == "dotnet")
            start.FileName = Path.Combine(directory, "dotnet", "dotnet");
        start.Environment["WASI_SDK_PATH"] = Path.Combine(directory, "wasi-sdk");
        start.Environment["WISP_GUEST_RUNTIME_FEED"] = Path.Combine(directory, "feed");
        start.Environment["WISP_CLI"] = Path.Combine(directory, "bin", "wisp");
        start.Environment["PATH"] = Path.Combine(directory, "bin") + Path.PathSeparator
            + Path.Combine(directory, "dotnet") + Path.PathSeparator + (start.Environment.TryGetValue("PATH", out var searchPath) ? searchPath : "");
    }

    internal static async Task<string> DiagnoseAsync(string home, CancellationToken cancellation, string? bundledRuntime = null)
    {
        var current = Path.Combine(home, "current.json");
        if (!File.Exists(current))
            throw new InvalidOperationException("No toolchain is installed. Run isoplane setup --source DIRECTORY using the preview toolchain bundle.");
        var directory = JsonSerializer.Deserialize<string>(await File.ReadAllTextAsync(current, cancellation))
            ?? throw new InvalidOperationException("The toolchain selection is invalid; rerun isoplane setup.");
        var manifest = ReadManifest(await File.ReadAllBytesAsync(Path.Combine(directory, ManifestName), cancellation));
        ValidatePlatform(manifest);
        await VerifyAsync(directory, manifest, cancellation);
        if (await File.ReadAllTextAsync(Path.Combine(home, "toolchain.props"), cancellation) != Properties(directory))
            throw new InvalidOperationException("The installed MSBuild toolchain selection is missing or inconsistent; rerun isoplane setup.");
        if (new DirectoryInfo(Path.Combine(home, "dotnet")).ResolveLinkTarget(true)?.FullName != Path.Combine(directory, "dotnet"))
            throw new InvalidOperationException("The installed SDK selection link is inconsistent; rerun isoplane setup.");
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
                throw new InvalidOperationException($"Toolchain file {path} is corrupt. Rerun isoplane setup --source DIRECTORY to repair it.");
            if (!OperatingSystem.IsWindows() && file.Executable && (File.GetUnixFileMode(path) & UnixFileMode.UserExecute) == 0)
                throw new InvalidOperationException($"Toolchain file {path} is not executable; rerun setup from a complete bundle.");
        }
        await VerifyCompilerPackageAsync(directory, manifest, cancellation);
    }

    private static async Task VerifyCompilerPackageAsync(string directory, ToolchainManifest manifest, CancellationToken cancellation)
    {
        using var receipt = JsonDocument.Parse(await File.ReadAllTextAsync(Path.Combine(directory, "compiler", "compiler.json"), cancellation));
        var package = receipt.RootElement.GetProperty("package").GetString();
        if (string.IsNullOrEmpty(package) || Path.GetFileName(package) != package || package.Contains('\\'))
            throw new InvalidOperationException("The compiler receipt has an invalid package path; reinstall the supplied toolchain.");
        var entry = manifest.Files.SingleOrDefault(file => file.Path == "compiler/" + package);
        if (entry is null || entry.Sha256 != receipt.RootElement.GetProperty("package_sha256").GetString())
            throw new InvalidOperationException("The compiler package is missing from the toolchain manifest or has the wrong identity; reinstall the complete bundle.");
        var backend = manifest.Files.Single(file => file.Path == "compiler/backend");
        if (backend.Sha256 != receipt.RootElement.GetProperty("backend_sha256").GetString())
            throw new InvalidOperationException("The compiler dependency probe differs from the packaged backend; reinstall the complete bundle.");
    }

    private static async Task VerifyRuntimePairAsync(ToolchainManifest manifest, string bundledRuntime, CancellationToken cancellation)
    {
        foreach (var binary in new[] { "wisp", "wisp-process" })
        {
            await using var stream = File.OpenRead(Path.Combine(bundledRuntime, binary));
            var digest = Convert.ToHexStringLower(await SHA256.HashDataAsync(stream, cancellation));
            var expected = manifest.Files.Single(file => file.Path == "bin/" + binary).Sha256;
            if (!string.Equals(digest, expected, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException($"The CLI and toolchain contain different {binary} binaries. Install the CLI and run setup from the same preview release.");
        }
    }

    private static ToolchainManifest ReadManifest(byte[] bytes)
    {
        var manifest = JsonSerializer.Deserialize<ToolchainManifest>(bytes, Json)
            ?? throw new InvalidOperationException("The toolchain bundle has no manifest.");
        if (string.IsNullOrWhiteSpace(manifest.RuntimeIdentifier) || string.IsNullOrWhiteSpace(manifest.DotnetSdkVersion) || string.IsNullOrWhiteSpace(manifest.WasiWorkloadVersion) || manifest.Files is not { Length: > 0 })
            throw new InvalidOperationException("The toolchain manifest must identify its SDK and files.");
        var names = new HashSet<string>(StringComparer.Ordinal);
        foreach (var file in manifest.Files)
        {
            if (file is null || string.IsNullOrWhiteSpace(file.Path) || string.IsNullOrWhiteSpace(file.Sha256)
                || !names.Add(file.Path) || file.Sha256.Length != 64 || !file.Sha256.All(Uri.IsHexDigit))
                throw new InvalidOperationException($"Invalid or duplicate toolchain manifest entry: {file?.Path ?? "<null>"}.");
            Resolve(Path.GetTempPath(), file.Path);
        }
        foreach (var required in RequiredFiles(manifest.DotnetSdkVersion, manifest.WasiWorkloadVersion, manifest.RuntimeIdentifier))
            if (!names.Contains(required))
                throw new InvalidOperationException($"The toolchain manifest is incomplete: missing {required}.");
        return manifest;
    }

    internal static string[] RequiredFiles(string sdk, string workload, string rid) => [
        "bin/wisp", "bin/wisp-process", "bin/wasm-tools", "wasi-sdk/bin/clang",
        "dotnet/dotnet", $"dotnet/sdk/{sdk}/dotnet.dll", "feed/Wisp.GuestRuntime.props",
        "compiler/compiler.json", "compiler/Wisp.GuestCompiler.props", "compiler/backend",
        $"dotnet/packs/Microsoft.NET.Runtime.MonoTargets.Sdk/{workload}/Sdk/Sdk.props",
        $"dotnet/packs/Microsoft.NETCore.App.Runtime.AOT.{rid}.Cross.wasi-wasm/{workload}/Sdk/Sdk.props"
    ];

    internal static void ProbeCompiler(string directory)
    {
        var path = Path.Combine(directory, "compiler", "backend");
        try
        {
            var library = NativeLibrary.Load(path);
            NativeLibrary.Free(library);
        }
        catch (Exception error) when (error is DllNotFoundException or BadImageFormatException)
        {
            throw new InvalidOperationException($"The NativeAOT compiler backend cannot load. Install the native dependencies listed in the toolchain README. {error.Message}", error);
        }
    }

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

    private static string Resolve(string directory, string relative)
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

    private static void SelectDotnet(string home, string target)
    {
        var selected = Path.Combine(home, "dotnet");
        if (Directory.Exists(selected) && new DirectoryInfo(selected).LinkTarget is null)
            throw new InvalidOperationException($"{selected} is a directory rather than an Isoplane SDK selection link; move it before running setup.");
        var temporary = selected + "." + Guid.NewGuid().ToString("N");
        try
        {
            Directory.CreateSymbolicLink(temporary, target);
            if (new DirectoryInfo(selected).LinkTarget is not null)
                Directory.Delete(selected);
            Directory.Move(temporary, selected);
        }
        finally
        {
            if (new DirectoryInfo(temporary).LinkTarget is not null)
                Directory.Delete(temporary);
        }
    }

    private static string Properties(string directory) => new XDocument(new XElement("Project", new XElement("PropertyGroup",
        Property("WispGuestRuntimeFeed", Path.Combine(directory, "feed")),
        Property("WASI_SDK_PATH", Path.Combine(directory, "wasi-sdk")),
        Property("WispCli", Path.Combine(directory, "bin", "wisp"))))).ToString();

    private static XElement Property(string name, string value) => new(name, new XAttribute("Condition", $"'$({name})' == ''"), value);

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
