using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Isoplane.Native;

namespace Isoplane.Cli;

internal sealed record SetupOptions(string? Source, bool AssumeYes)
{
    internal static SetupOptions Parse(string[] args)
    {
        string? source = null;
        var yes = false;
        for (var index = 1; index < args.Length; index++)
        {
            switch (args[index])
            {
                case "--source" when index + 1 < args.Length: source = args[++index]; break;
                case "--yes" or "-y": yes = true; break;
                default: throw new ArgumentException($"Unknown setup option '{args[index]}'. Usage: isoplane setup [--source DIRECTORY|URL] [--yes]");
            }
        }
        return new(source, yes);
    }
}

/// <summary>The machine-facing pieces of setup that tests replace: what the loader can find, who answers prompts, how privileged commands run.</summary>
internal sealed record SetupMachine(
    Func<string, bool> CanLoad,
    Func<string, bool> FileExists,
    OperatingSystemRelease? LinuxDistribution,
    Func<string, Task<bool>> Confirm,
    Func<string, CancellationToken, Task<int>> RunPrivileged,
    Func<string, ISdkRegistrations> Registrations,
    Func<string, CancellationToken, Task> ProveReady,
    string? BundledRuntime,
    Func<bool, ISdkInstallation> Installers)
{
    internal static SetupMachine Production() => new(
        Prerequisites.CanLoad,
        File.Exists,
        OperatingSystem.IsLinux() ? Prerequisites.Detect() : null,
        ConfirmOnTerminal,
        (command, cancellation) => Processes.RunAsync("/bin/sh", ["-c", command], Environment.CurrentDirectory, cancellation),
        dotnet => new DotnetRegistrations(dotnet, Toolchain.Home),
        Readiness.ProveAsync,
        Path.Combine(AppContext.BaseDirectory, "runtime"),
        SdkInstallation.Create);

    internal static Task<bool> ConfirmOnTerminal(string question)
    {
        if (Console.IsInputRedirected)
            return Task.FromResult(false);
        Console.Error.Write($"{question} [y/N] ");
        return Task.FromResult(Console.ReadLine()?.Trim().ToLowerInvariant() is "y" or "yes");
    }
}

/// <summary>
/// Installs the running CLI's matching release through the platform installer,
/// verifies its assets, records ownership, and proves readiness before reporting success.
/// </summary>
internal static class Setup
{
    internal const string PackageSourceName = "isoplane";

    internal static async Task<int> RunAsync(SetupOptions options, ReleaseIdentity cli, string home, SetupMachine machine, HttpClient client, CancellationToken cancellation)
    {
        home = Path.GetFullPath(home);
        var platform = Toolchain.PlatformIdentifier();
        if (platform is not ("linux-x64" or "win-x64" or "osx-arm64"))
            throw new InvalidOperationException($"Isoplane releases support Linux x64 (glibc), Windows x64, and macOS ARM64; this machine is {platform}.");
        if (machine.LinuxDistribution is { } distribution)
            await EnsurePrerequisitesAsync(options, machine, distribution, cancellation);

        var source = options.Source is null
            ? ReleaseSource.AtOrigin(Release.Origin(), cli.ReleaseName, client)
            : ReleaseSource.Open(options.Source, client);
        Console.Error.WriteLine($"Reading the release manifest from {source.Location}...");
        var manifest = await Release.ReadAsync(source, cancellation);
        Release.RequireThisPlatform(manifest, source);
        Release.RequireSameRelease(manifest, cli, source);

        Directory.CreateDirectory(home);
        using var held = new FileStream(Path.Combine(home, "setup.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        var installation = Installation.Begin(home, cli);

        var packages = Path.Combine(home, "packages");
        RecordDirectory(installation, packages);
        foreach (var package in manifest.Packages)
        {
            var destination = Path.Combine(packages, Path.GetFileName(package.File));
            RecordFile(installation, destination, package.Sha256);
            if (await source.FetchAsync(package.File, destination, package.Sha256, null, cancellation))
                Console.Error.WriteLine($"Installed package {package.Id} {package.Version}.");
        }

        var identity = manifest.Toolchain.ManifestSha256;
        RecordDirectory(installation, Toolchain.Installations(home));
        var toolchain = await Toolchain.VerifiedInstallationAsync(home, identity, cancellation);
        if (toolchain is null)
        {
            var downloads = Path.Combine(home, "downloads");
            RecordDirectory(installation, downloads);
            var download = Path.Combine(downloads, Guid.NewGuid().ToString("N"));
            RecordDirectory(installation, download);
            var archive = Path.Combine(download, Path.GetFileName(manifest.Toolchain.Archive));
            if (await source.FetchAsync(manifest.Toolchain.Archive, archive, manifest.Toolchain.ArchiveSha256, new DownloadProgress(manifest.Toolchain.Archive), cancellation))
                Console.Error.WriteLine($"Downloaded {manifest.Toolchain.Archive}.");
            Console.Error.WriteLine("Unpacking the SDK bundle...");
            var destination = Path.Combine(Toolchain.Installations(home), identity);
            if (Directory.Exists(destination) && installation.Find(InstalledResourceKind.Directory, destination) is not { Owned: true })
                throw new InvalidOperationException($"Setup will not replace the unowned directory {destination}. Move it aside and retry.");
            RecordDirectory(installation, destination);
            toolchain = await Toolchain.InstallArchiveAsync(archive, identity, home, cancellation);
            foreach (var partial in installation.Resources.Where(resource => resource is { Kind: InstalledResourceKind.Directory, Owned: true }
                && Path.GetDirectoryName(resource.Target) == downloads).ToArray())
            {
                if (Directory.Exists(partial.Target))
                    Directory.Delete(partial.Target, recursive: true);
                installation.Forget(partial);
            }
            if (installation.Find(InstalledResourceKind.Directory, downloads) is { Owned: true } recordedDownload && !Directory.EnumerateFileSystemEntries(downloads).Any())
            {
                Directory.Delete(downloads);
                installation.Forget(recordedDownload);
            }
        }
        else
        {
            RecordDirectory(installation, toolchain);
            Console.Error.WriteLine("The SDK bundle is already downloaded and verified.");
        }
        var bundle = toolchain;
        var bundled = Toolchain.ReadManifest(await File.ReadAllBytesAsync(Path.Combine(bundle, Toolchain.ManifestName), cancellation));
        foreach (var package in bundled.Files.Where(file => file.Path.StartsWith("packages/", StringComparison.Ordinal)))
        {
            var destination = Path.Combine(packages, Path.GetFileName(package.Path));
            RecordFile(installation, destination, package.Sha256);
            if (!MatchesFile(destination, package.Sha256))
                File.Copy(Toolchain.Resolve(bundle, package.Path), destination, overwrite: true);
        }
        toolchain = await machine.Installers(options.AssumeYes).InstallAsync(bundle, manifest.PackageVersion, installation, cancellation);
        var selection = Path.Combine(home, "current.json");
        var selectionDigest = Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(JsonSerializer.Serialize(toolchain))));
        RecordFile(installation, selection, selectionDigest);
        await Toolchain.SelectAsync(home, toolchain, cancellation);
        RemoveSupersededToolchains(installation, home, bundle);

        var registrations = machine.Registrations(DotnetRegistrations.BootstrapDotnet());
        await RegisterPackageSourceAsync(installation, registrations, packages, cancellation);

        Console.Error.WriteLine("Checking that the installed tools run...");
        await Toolchain.DiagnoseAsync(home, cancellation, machine.BundledRuntime);
        await machine.ProveReady(toolchain, cancellation);

        Console.WriteLine($"Installed Isoplane {manifest.PackageVersion}. Your .NET SDK is ready to build Isoplane applications.");
        Console.WriteLine("Create an application with: dotnet new isoplane-web --name MyApp");
        return 0;
    }

    private static void RecordDirectory(Installation installation, string directory)
    {
        var owned = installation.Find(InstalledResourceKind.Directory, directory)?.Owned ?? !Directory.Exists(directory);
        installation.Record(new(InstalledResourceKind.Directory, directory, owned));
    }

    private static void RecordFile(Installation installation, string path, string digest)
    {
        var recorded = installation.Find(InstalledResourceKind.File, path);
        var owned = recorded?.Owned ?? !File.Exists(path);
        if (!owned && !MatchesFile(path, digest))
            throw new InvalidOperationException($"Setup will not replace the unowned file {path}. Move it aside and retry.");
        installation.Record(new(InstalledResourceKind.File, path, owned, digest));
    }

    private static bool MatchesFile(string path, string digest)
    {
        if (!File.Exists(path))
            return false;
        using var stream = File.OpenRead(path);
        return Convert.ToHexStringLower(SHA256.HashData(stream)) == digest;
    }

    private static async Task EnsurePrerequisitesAsync(SetupOptions options, SetupMachine machine, OperatingSystemRelease distribution, CancellationToken cancellation)
    {
        var missing = Prerequisites.Missing(machine.CanLoad, machine.FileExists);
        if (missing.Count == 0)
            return;
        var described = Prerequisites.Describe(missing);
        var command = Prerequisites.InstallCommand(distribution, missing)
            ?? throw new InvalidOperationException($"Setup is incomplete: this machine lacks {described}. Install your distribution's packages providing them (on Ubuntu 24.04: {string.Join(' ', missing.Select(m => m.UbuntuPackage))}), then rerun isoplane setup.");
        Console.Error.WriteLine($"This machine lacks {described}.");
        if (!options.AssumeYes && !await machine.Confirm($"Install them now with: {command}"))
            throw new InvalidOperationException($"Setup is incomplete: the missing prerequisites were not installed. Run this, then rerun isoplane setup (or pass --yes to let setup run it):{Environment.NewLine}  {command}");
        Console.Error.WriteLine($"Running: {command}");
        var exit = await machine.RunPrivileged(command, cancellation);
        var remaining = Prerequisites.Missing(machine.CanLoad, machine.FileExists);
        if (exit != 0 || remaining.Count > 0)
            throw new InvalidOperationException($"Setup is incomplete: '{command}' exited {exit} and this machine still lacks {(remaining.Count > 0 ? Prerequisites.Describe(remaining) : "nothing")}. Resolve the package installation, then rerun isoplane setup.");
    }

    private static void RemoveSupersededToolchains(Installation installation, string home, string current)
    {
        foreach (var directory in Directory.GetDirectories(Toolchain.Installations(home)))
        {
            if (directory == current || Path.GetFileName(directory).StartsWith('.'))
                continue;
            var recorded = installation.Find(InstalledResourceKind.Directory, directory);
            if (recorded is not { Owned: true })
                continue;
            Directory.Delete(directory, recursive: true);
            installation.Forget(recorded);
            Console.Error.WriteLine($"Removed the superseded toolchain {Path.GetFileName(directory)}.");
        }
    }

    private static async Task RegisterPackageSourceAsync(Installation installation, ISdkRegistrations registrations, string packages, CancellationToken cancellation)
    {
        var existing = await registrations.PackageSourceAsync(PackageSourceName, cancellation);
        var recorded = installation.Find(InstalledResourceKind.PackageSource, PackageSourceName);
        if (existing is not null && Path.GetFullPath(existing.TrimEnd(Path.DirectorySeparatorChar)) == packages)
        {
            installation.Record(new(InstalledResourceKind.PackageSource, PackageSourceName, Owned: recorded?.Owned ?? false, Detail: existing));
            return;
        }
        if (existing is not null)
        {
            if (recorded is not { Owned: true })
                throw new InvalidOperationException($"A NuGet package source named '{PackageSourceName}' already points at {existing}. Remove or rename it (dotnet nuget remove source {PackageSourceName}), then rerun isoplane setup.");
            await registrations.RemovePackageSourceAsync(PackageSourceName, cancellation);
        }
        installation.Record(new(InstalledResourceKind.PackageSource, PackageSourceName, Owned: true, Detail: packages));
        await registrations.AddPackageSourceAsync(PackageSourceName, packages, cancellation);
        Console.Error.WriteLine($"Registered the NuGet package source '{PackageSourceName}' at {packages}.");
    }

    private sealed class DownloadProgress(string name) : IProgress<(long Copied, long? Total)>
    {
        private int lastPercent = -1;

        public void Report((long Copied, long? Total) value)
        {
            var percent = value.Total is > 0 ? (int)(value.Copied * 100 / value.Total.Value) : -1;
            if (percent == lastPercent || (percent % 10 != 0 && Console.IsErrorRedirected))
                return;
            lastPercent = percent;
            var size = value.Total is { } total ? $"{total / (1024 * 1024)} MB" : "unknown size";
            if (Console.IsErrorRedirected)
                Console.Error.WriteLine($"Downloading {name} ({size}): {percent}%");
            else
                Console.Error.Write($"\rDownloading {name} ({size}): {percent}%{(percent == 100 ? Environment.NewLine : "")}");
        }
    }
}

/// <summary>Proves an installed toolchain runs on this machine by running its tools, as doctor does.</summary>
internal static class Readiness
{
    internal static async Task ProveAsync(string toolchain, CancellationToken cancellation)
    {
        Toolchain.ProbeCompiler(toolchain);
        NativeRuntime.EnsureLoaded();
        foreach (var (name, path) in Probes)
        {
            Console.Error.WriteLine($"Checking {name}...");
            await Processes.CheckAsync(Path.Combine(toolchain, path), ["--version"], toolchain, cancellation);
        }
    }

    internal static readonly (string Name, string Path)[] Probes =
    [
        ("WebAssembly compiler", "wasi-sdk/bin/clang" + Toolchain.ExecutableExtension),
        ("WebAssembly tools", "bin/wasm-tools" + Toolchain.ExecutableExtension),
    ];
}
