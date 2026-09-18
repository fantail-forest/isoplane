using Wisp.Native;

namespace Wisp.Cli;

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
    OperatingSystemRelease OperatingSystem,
    Func<string, Task<bool>> Confirm,
    Func<string, CancellationToken, Task<int>> RunPrivileged,
    Func<string, ISdkRegistrations> Registrations,
    Func<string, CancellationToken, Task> ProveReady,
    string UserHome,
    string? BundledRuntime)
{
    internal static SetupMachine Production() => new(
        Prerequisites.CanLoad,
        File.Exists,
        Prerequisites.Detect(),
        ConfirmOnTerminal,
        (command, cancellation) => Processes.RunAsync("/bin/sh", ["-c", command], Environment.CurrentDirectory, cancellation),
        dotnet => new DotnetRegistrations(dotnet, Toolchain.Home),
        Readiness.ProveAsync,
        Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
        Path.Combine(AppContext.BaseDirectory, "runtime"));

    private static Task<bool> ConfirmOnTerminal(string question)
    {
        if (Console.IsInputRedirected)
            return Task.FromResult(false);
        Console.Error.Write($"{question} [y/N] ");
        return Task.FromResult(Console.ReadLine()?.Trim().ToLowerInvariant() is "y" or "yes");
    }
}

/// <summary>
/// <c>isoplane setup</c>: installs the release the running CLI belongs to. Prerequisites are checked before anything is
/// downloaded; every asset is verified against the release manifest; every change is recorded in the installation
/// manifest as it is made; readiness is proven by running the installed tools before success is reported.
/// </summary>
internal static class Setup
{
    internal const string PackageSourceName = "isoplane";
    internal const string TemplatePackageId = "Wisp.Templates";

    internal static async Task<int> RunAsync(SetupOptions options, ReleaseIdentity cli, string home, SetupMachine machine, HttpClient client, CancellationToken cancellation)
    {
        home = Path.GetFullPath(home);
        var platform = Toolchain.PlatformIdentifier();
        if (platform != "linux-x64")
            throw new InvalidOperationException($"Isoplane releases support Linux x64 (glibc); this machine is {platform}.");
        await EnsurePrerequisitesAsync(options, machine, cancellation);

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
        installation.Record(new(InstalledResourceKind.Directory, packages, Owned: true));
        foreach (var package in manifest.Packages)
        {
            var destination = Path.Combine(packages, Path.GetFileName(package.File));
            installation.Record(new(InstalledResourceKind.File, destination, Owned: true, Detail: package.Sha256));
            if (await source.FetchAsync(package.File, destination, package.Sha256, null, cancellation))
                Console.Error.WriteLine($"Installed package {package.Id} {package.Version}.");
        }

        var identity = manifest.Toolchain.ManifestSha256;
        installation.Record(new(InstalledResourceKind.Directory, Toolchain.Installations(home), Owned: true));
        var toolchain = await Toolchain.VerifiedInstallationAsync(home, identity, cancellation, machine.BundledRuntime);
        if (toolchain is null)
        {
            var downloads = Path.Combine(home, "downloads");
            installation.Record(new(InstalledResourceKind.Directory, downloads, Owned: true));
            var archive = Path.Combine(downloads, cli.ReleaseName, Path.GetFileName(manifest.Toolchain.Archive));
            if (await source.FetchAsync(manifest.Toolchain.Archive, archive, manifest.Toolchain.ArchiveSha256, new DownloadProgress(manifest.Toolchain.Archive), cancellation))
                Console.Error.WriteLine($"Downloaded {manifest.Toolchain.Archive}.");
            Console.Error.WriteLine("Installing the toolchain...");
            installation.Record(new(InstalledResourceKind.Directory, Path.Combine(Toolchain.Installations(home), identity), Owned: true, Detail: manifest.PackageVersion));
            toolchain = await Toolchain.InstallArchiveAsync(archive, identity, home, cancellation, machine.BundledRuntime);
            Directory.Delete(downloads, recursive: true);
            installation.Forget(new(InstalledResourceKind.Directory, downloads, Owned: true));
        }
        else
        {
            installation.Record(new(InstalledResourceKind.Directory, toolchain, Owned: true, Detail: manifest.PackageVersion));
            Console.Error.WriteLine("The toolchain is already installed and verified.");
        }
        var (link, properties, selection) = Toolchain.SelectionPaths(home);
        installation.Record(new(InstalledResourceKind.SymbolicLink, link, Owned: true));
        installation.Record(new(InstalledResourceKind.File, properties, Owned: true));
        installation.Record(new(InstalledResourceKind.File, selection, Owned: true));
        await Toolchain.SelectAsync(home, toolchain, cancellation);
        RemoveSupersededToolchains(installation, home, toolchain);

        var registrations = machine.Registrations(Path.Combine(toolchain, "dotnet", "dotnet"));
        await RegisterPackageSourceAsync(installation, registrations, packages, cancellation);
        var template = manifest.Packages.SingleOrDefault(package => package.Id == TemplatePackageId)
            ?? throw new InvalidOperationException($"The release carries no {TemplatePackageId} package; the release is incomplete.");
        await RegisterTemplateAsync(installation, registrations, Path.Combine(packages, Path.GetFileName(template.File)), template.Version, cancellation);
        ConfigureShell(installation, home, machine.UserHome);

        Console.Error.WriteLine("Checking that the installed tools run...");
        await machine.ProveReady(toolchain, cancellation);

        Console.WriteLine($"Installed Isoplane {manifest.PackageVersion} under {home}.");
        Console.WriteLine($"New terminals find dotnet and isoplane through {ShellEnvironment.EnvironmentFile(home)}; in this one, run: . \"{ShellEnvironment.EnvironmentFile(home)}\"");
        Console.WriteLine("Create an application with: dotnet new isoplane-web --name MyApp");
        return 0;
    }

    private static async Task EnsurePrerequisitesAsync(SetupOptions options, SetupMachine machine, CancellationToken cancellation)
    {
        var missing = Prerequisites.Missing(machine.CanLoad, machine.FileExists);
        if (missing.Count == 0)
            return;
        var described = Prerequisites.Describe(missing);
        var command = Prerequisites.InstallCommand(machine.OperatingSystem, missing)
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

    private static async Task RegisterTemplateAsync(Installation installation, ISdkRegistrations registrations, string package, string version, CancellationToken cancellation)
    {
        var registered = await registrations.TemplateVersionAsync(TemplatePackageId, cancellation);
        if (registered == version && installation.Find(InstalledResourceKind.Template, TemplatePackageId) is not null)
            return;
        installation.Record(new(InstalledResourceKind.Template, TemplatePackageId, Owned: true, Detail: version));
        if (registered is not null)
            await registrations.UninstallTemplateAsync(TemplatePackageId, cancellation);
        await registrations.InstallTemplateAsync(package, cancellation);
        Console.Error.WriteLine($"Registered the project templates from {TemplatePackageId} {version}.");
    }

    private static void ConfigureShell(Installation installation, string home, string userHome)
    {
        var environment = ShellEnvironment.EnvironmentFile(home);
        installation.Record(new(InstalledResourceKind.File, environment, Owned: true));
        File.WriteAllText(environment, ShellEnvironment.Script(home, userHome));
        var line = ShellEnvironment.SourceLine(home, userHome);
        foreach (var file in ShellEnvironment.StartupFiles(userHome))
        {
            var recorded = installation.Find(InstalledResourceKind.ProfileLine, file);
            var owned = recorded?.Owned ?? true;
            installation.Record(new(InstalledResourceKind.ProfileLine, file, Owned: owned, Detail: line));
            ShellEnvironment.AddLine(file, line);
        }
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
        (".NET SDK", "dotnet/dotnet"),
        ("WebAssembly compiler", "wasi-sdk/bin/clang"),
        ("WebAssembly tools", "bin/wasm-tools"),
    ];
}
