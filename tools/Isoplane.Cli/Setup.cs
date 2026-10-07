using System.Security.Cryptography;
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
    Func<CancellationToken, Task> ProveReady)
{
    internal static SetupMachine Production() => new(
        Prerequisites.CanLoad,
        File.Exists,
        OperatingSystem.IsLinux() ? Prerequisites.Detect() : null,
        ConfirmOnTerminal,
        (command, cancellation) => Processes.RunAsync("/bin/sh", ["-c", command], Environment.CurrentDirectory, cancellation),
        dotnet => new DotnetRegistrations(dotnet, CliPaths.Home),
        Readiness.ProveAsync);

    internal static Task<bool> ConfirmOnTerminal(string question)
    {
        if (Console.IsInputRedirected)
            return Task.FromResult(false);
        Console.Error.Write($"{question} [y/N] ");
        return Task.FromResult(Console.ReadLine()?.Trim().ToLowerInvariant() is "y" or "yes");
    }
}

/// <summary>
/// Checks host prerequisites and registers templates for the running CLI.
/// An explicit release source also registers its verified packages for offline development.
/// </summary>
internal static class Setup
{
    internal const string PackageSourceName = "isoplane";

    internal static async Task<int> RunAsync(SetupOptions options, ReleaseIdentity cli, string home, SetupMachine machine, HttpClient client, CancellationToken cancellation)
    {
        home = Path.GetFullPath(home);
        var platform = CliPaths.PlatformIdentifier();
        if (platform is not ("linux-x64" or "win-x64" or "osx-arm64"))
            throw new InvalidOperationException($"Isoplane releases support Linux x64 (glibc), Windows x64, and macOS ARM64; this machine is {platform}.");
        if (machine.LinuxDistribution is { } distribution)
            await EnsurePrerequisitesAsync(options, machine, distribution, cancellation);

        await machine.ProveReady(cancellation);
        Directory.CreateDirectory(home);
        using var held = new FileStream(Path.Combine(home, "setup.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        var installation = Installation.Begin(home, cli);
        var registrations = machine.Registrations(DotnetRegistrations.BootstrapDotnet());
        var templateSource = $"Isoplane.Templates::{cli.Version}";
        if (options.Source is not null)
        {
            var source = ReleaseSource.Open(options.Source, client);
            var manifest = await Release.ReadAsync(source, cancellation);
            Release.RequireThisPlatform(manifest, source);
            Release.RequireSameRelease(manifest, cli, source);
            var packages = Path.Combine(home, "packages");
            RecordDirectory(installation, packages);
            foreach (var package in manifest.Packages)
            {
                var destination = Path.Combine(packages, Path.GetFileName(package.File));
                RecordFile(installation, destination, package.Sha256);
                await source.FetchAsync(package.File, destination, package.Sha256, cancellation);
            }
            await RegisterPackageSourceAsync(installation, registrations, packages, cancellation);
            templateSource = Path.Combine(packages, $"Isoplane.Templates.{cli.Version}.nupkg");
        }
        await RegisterTemplatesAsync(installation, registrations, templateSource, cli.Version, cancellation);
        Console.WriteLine($"Isoplane {cli.Version} is ready. Create an application with: dotnet new isoplane-web --name MyApp");
        return 0;
    }

    private static async Task RegisterTemplatesAsync(Installation installation, ISdkRegistrations registrations, string source, string version, CancellationToken cancellation)
    {
        const string id = "Isoplane.Templates";
        var existing = await registrations.TemplateVersionAsync(id, cancellation);
        var recorded = installation.Find(InstalledResourceKind.Template, id);
        if (existing != version)
        {
            if (existing is not null && recorded is not { Owned: true })
                throw new InvalidOperationException("An Isoplane template installed outside setup already exists at another version. Remove it with dotnet new uninstall Isoplane.Templates, then retry setup.");
            installation.Record(new(InstalledResourceKind.Template, id, true, version));
            await registrations.InstallTemplateAsync(source, cancellation);
            if (await registrations.TemplateVersionAsync(id, cancellation) != version)
                throw new InvalidOperationException($"Template installation did not register Isoplane.Templates {version}. Rerun isoplane setup.");
        }
        else
            installation.Record(new(InstalledResourceKind.Template, id, recorded?.Owned ?? false, version));
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

}

/// <summary>Checks the selected .NET SDK and the native library shipped with the CLI.</summary>
internal static class Readiness
{
    internal static void RequireSdkVersion(string version)
    {
        if (!System.Version.TryParse(version.Split('-')[0], out var parsed) || parsed.Major != 10 || parsed.Minor != 0)
            throw new InvalidOperationException($"Isoplane requires a .NET 10 SDK; the selected dotnet reports '{version}'. Install a .NET 10 SDK and retry.");
    }

    internal static async Task ProveAsync(CancellationToken cancellation)
    {
        var dotnet = DotnetRegistrations.BootstrapDotnet();
        var version = await new DotnetRegistrations(dotnet, Environment.CurrentDirectory).SdkVersionAsync(cancellation);
        RequireSdkVersion(version.Trim());
        if (OperatingSystem.IsWindows() && new[] { "msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll" }.Any(name => !Prerequisites.CanLoad(name)))
            throw new InvalidOperationException("Install the Microsoft Visual C++ x64 Redistributable, then rerun isoplane setup: https://aka.ms/vs/17/release/vc_redist.x64.exe");
        NativeRuntime.EnsureLoaded();
    }
}
