using System.Text.Json;

namespace Isoplane.Cli;

internal sealed record WorkloadSdk(string Dotnet, string Version, string FeatureBand, string Root);

internal interface ISdkInstallation
{
    Task<string> InstallAsync(string bundle, string version, Installation installation, CancellationToken cancellation);
    Task RemoveAsync(InstalledResource resource, CancellationToken cancellation);
}

internal static class SdkInstallation
{
    internal static ISdkInstallation Create(bool assumeYes) => OperatingSystem.IsWindows()
        ? new WindowsInstallation()
        : new WorkloadInstallation(DotnetRegistrations.BootstrapDotnet(), SetupMachine.ConfirmOnTerminal, assumeYes);
}

/// <summary>Installs Isoplane packs through the selected SDK's file-based workload manager.</summary>
internal sealed class WorkloadInstallation(string dotnet, Func<string, Task<bool>> confirm, bool assumeYes) : ISdkInstallation
{
    internal const string Id = "isoplane";

    internal async Task<WorkloadSdk> DiscoverAsync(CancellationToken cancellation)
    {
        var version = (await RunAsync(dotnet, ["--version"], Path.GetTempPath(), cancellation)).Trim();
        var sdkRoot = Path.GetDirectoryName(Path.GetFullPath(dotnet))!;
        var band = FeatureBand(version);
        var userLocal = File.Exists(Path.Combine(sdkRoot, "metadata", "workloads", band, "userlocal"));
        var root = userLocal
            ? Path.Combine(Environment.GetEnvironmentVariable("DOTNET_CLI_HOME")
                ?? Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".dotnet")
            : sdkRoot;
        return new(dotnet, version, band, root);
    }

    internal static string FeatureBand(string version)
    {
        if (!System.Version.TryParse(version.Split('-')[0], out var parsed) || parsed.Major != 10 || parsed.Minor != 0 || parsed.Build < 0)
            throw new InvalidOperationException($"Isoplane requires a .NET 10 SDK; the selected dotnet reports '{version}'. Install a .NET 10 SDK, then run isoplane setup again.");
        return $"{parsed.Major}.{parsed.Minor}.{parsed.Build / 100 * 100}";
    }

    public async Task<string> InstallAsync(string bundle, string version, Installation installation, CancellationToken cancellation)
    {
        var sdk = await DiscoverAsync(cancellation);
        using var manifest = JsonDocument.Parse(await File.ReadAllTextAsync(Path.Combine(bundle, "WorkloadManifest.json"), cancellation));
        if (manifest.RootElement.GetProperty("version").GetString() != version)
            throw new InvalidOperationException("The workload manifest does not match this release. Download a complete matching release and retry setup.");
        var directory = Path.Combine(sdk.Root, "sdk-manifests", sdk.FeatureBand, Id, version);
        var pack = Path.Combine(sdk.Root, "packs", "Isoplane.Toolchain." + Toolchain.PlatformIdentifier(), version);
        var recordPath = Path.Combine(sdk.Root, "metadata", "workloads", sdk.FeatureBand, "InstalledWorkloads", Id);
        var existing = installation.Find(InstalledResourceKind.Workload, recordPath);
        var reused = File.Exists(recordPath) && existing is not { Owned: true };
        if (reused && (!Directory.Exists(directory) || !Directory.Exists(pack)))
            throw new InvalidOperationException("An Isoplane workload installed outside setup already exists at another version. Remove it with dotnet workload uninstall isoplane before switching to setup-managed installation.");
        var sameVersion = Directory.Exists(directory);
        var recorded = installation.Find(InstalledResourceKind.WorkloadManifest, directory);
        if (Directory.Exists(directory) && recorded is not { Owned: true })
        {
            foreach (var name in new[] { "WorkloadManifest.json", "WorkloadManifest.targets" })
                if (!File.Exists(Path.Combine(directory, name)) || !File.ReadAllBytes(Path.Combine(directory, name)).SequenceEqual(File.ReadAllBytes(Path.Combine(bundle, name))))
                    throw new InvalidOperationException($"The existing workload manifest at {directory} differs from this release. Setup will not replace an installation it does not own.");
        }
        var owned = recorded?.Owned ?? !Directory.Exists(directory);
        installation.Record(new(InstalledResourceKind.WorkloadManifest, directory, owned, JsonSerializer.Serialize(sdk)));
        var elevated = !reused && await RequiresElevationAsync(sdk.Root, cancellation);
        if (owned)
        {
            if (elevated)
                await RunAsync("sudo", ["--", "mkdir", "-p", directory], Path.GetTempPath(), cancellation);
            else
                Directory.CreateDirectory(directory);
            foreach (var name in new[] { "WorkloadManifest.json", "WorkloadManifest.targets" })
            {
                if (elevated)
                    await RunAsync("sudo", ["--", "install", "-m", "644", Path.Combine(bundle, name), Path.Combine(directory, name)], Path.GetTempPath(), cancellation);
                else
                    File.Copy(Path.Combine(bundle, name), Path.Combine(directory, name), overwrite: true);
            }
        }
        installation.Record(new(InstalledResourceKind.Workload, recordPath, existing?.Owned ?? !File.Exists(recordPath), JsonSerializer.Serialize(sdk)));
        if (reused)
            return pack;
        if (sameVersion && File.Exists(recordPath) && await NeedsRepairAsync(pack, cancellation))
        {
            Console.Error.WriteLine("Repairing the Isoplane workload...");
            await WorkloadCommandAsync(sdk, ["uninstall", Id], elevated, cancellation);
        }
        Console.Error.WriteLine($"Installing the Isoplane workload for .NET {sdk.Version}...");
        await WorkloadCommandAsync(sdk, ["install", Id, "--skip-manifest-update", "--source", Path.Combine(bundle, "packages")], elevated, cancellation);
        return pack;
    }

    public async Task RemoveAsync(InstalledResource resource, CancellationToken cancellation)
    {
        var sdk = JsonSerializer.Deserialize<WorkloadSdk>(resource.Detail ?? "null")
            ?? throw new InvalidOperationException("The installation record has no workload SDK information; restore the installation manifest before retrying removal.");
        var expected = resource.Kind == InstalledResourceKind.Workload
            ? Path.Combine(sdk.Root, "metadata", "workloads", sdk.FeatureBand, "InstalledWorkloads", Id)
            : Path.Combine(sdk.Root, "sdk-manifests", sdk.FeatureBand, Id);
        if ((resource.Kind == InstalledResourceKind.Workload ? resource.Target : Path.GetDirectoryName(resource.Target)) != expected)
            throw new InvalidOperationException("The installation record names an unexpected workload location; restore the installation manifest before retrying removal.");
        var elevated = await RequiresElevationAsync(sdk.Root, cancellation);
        if (resource.Kind == InstalledResourceKind.Workload)
        {
            if (File.Exists(resource.Target))
                await WorkloadCommandAsync(sdk, ["uninstall", Id], elevated, cancellation);
        }
        else if (Directory.Exists(resource.Target))
        {
            if (elevated)
                await RunAsync("sudo", ["--", "rm", "-r", "--", resource.Target], Path.GetTempPath(), cancellation);
            else
                Directory.Delete(resource.Target, recursive: true);
        }
    }

    private static async Task<bool> NeedsRepairAsync(string pack, CancellationToken cancellation)
    {
        try
        {
            var manifest = Toolchain.ReadManifest(await File.ReadAllBytesAsync(Path.Combine(pack, Toolchain.ManifestName), cancellation));
            await Toolchain.VerifyAsync(pack, manifest, cancellation);
            return false;
        }
        catch (Exception error) when (error is IOException or InvalidOperationException or JsonException)
        {
            return true;
        }
    }

    private async Task<bool> RequiresElevationAsync(string directory, CancellationToken cancellation)
    {
        DirectoryInfo? parent = new(directory);
        while (parent is not null && !parent.Exists)
            parent = parent.Parent;
        var probe = Path.Combine(parent?.FullName ?? directory, ".isoplane-write-" + Guid.NewGuid().ToString("N"));
        try
        {
            using var file = new FileStream(probe, FileMode.CreateNew, FileAccess.Write, FileShare.None, 1, FileOptions.DeleteOnClose);
            return false;
        }
        catch (UnauthorizedAccessException)
        {
            if (!assumeYes && !await confirm($"Installing or removing workloads in {directory} requires administrator access. Continue with sudo?"))
                throw new InvalidOperationException("Setup is incomplete: administrator access was declined. Run isoplane setup again when you can authorize workload installation.");
            if (await Processes.RunAsync("sudo", ["-v"], Path.GetTempPath(), cancellation) != 0)
                throw new InvalidOperationException("Administrator authorization failed; rerun isoplane setup when sudo access is available.");
            return true;
        }
    }

    private static async Task WorkloadCommandAsync(WorkloadSdk sdk, string[] arguments, bool elevated, CancellationToken cancellation)
    {
        var context = Directory.CreateTempSubdirectory("isoplane-workload-").FullName;
        try
        {
            await File.WriteAllTextAsync(Path.Combine(context, "global.json"), JsonSerializer.Serialize(new { sdk = new { version = sdk.Version, rollForward = "disable" } }), cancellation);
            var command = elevated ? "sudo" : sdk.Dotnet;
            string[] args = elevated ? ["--", sdk.Dotnet, "workload", .. arguments] : ["workload", .. arguments];
            var exitCode = await Processes.RunAsync(command, args, context, cancellation, redirectOutput: true);
            if (exitCode != 0)
                throw new InvalidOperationException($"The .NET workload command failed (exit {exitCode}). Resolve the error above and rerun the Isoplane command.");
        }
        finally
        {
            Directory.Delete(context, recursive: true);
        }
    }

    private static async Task<string> RunAsync(string command, string[] arguments, string directory, CancellationToken cancellation)
    {
        using var process = Processes.Start(command, arguments, directory, captureOutput: true, captureErrors: true);
        var output = process.StandardOutput.ReadToEndAsync(cancellation);
        var errors = process.StandardError.ReadToEndAsync(cancellation);
        try
        {
            await process.WaitForExitAsync(cancellation);
        }
        finally
        {
            await Processes.StopAsync(process);
        }
        var text = await output + await errors;
        if (process.ExitCode != 0)
            throw new InvalidOperationException($"{command} {string.Join(' ', arguments)} failed (exit {process.ExitCode}):{Environment.NewLine}{text}");
        return text;
    }
}
