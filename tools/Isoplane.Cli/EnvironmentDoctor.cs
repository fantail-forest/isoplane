using System.Text.Json;

namespace Isoplane.Cli;

internal static class EnvironmentDoctor
{
    internal static async Task<int> RunAsync(CancellationToken cancellation)
    {
        Console.Error.WriteLine("Checking toolchain integrity...");
        string installed;
        try
        {
            installed = await Toolchain.DiagnoseAsync(Toolchain.Home, cancellation, Path.Combine(AppContext.BaseDirectory, "runtime"));
        }
        catch (Exception error) when (error is IOException or JsonException)
        {
            throw new InvalidOperationException("The installed toolchain is missing or unreadable. Run isoplane setup to repair it.", error);
        }
        var installation = Installation.Load(Toolchain.Home)
            ?? throw new InvalidOperationException("The SDK installation record is missing. Run isoplane setup to repair it.");
        if (installation.CliVersion != ReleaseIdentity.OfRunningCli().Version)
            throw new InvalidOperationException("The CLI and installed SDK versions differ. Run isoplane setup to install this CLI's matching SDK.");
        var sdk = await Readiness.SelectedSdkAsync(cancellation);
        Console.Error.WriteLine($"Checking .NET SDK {sdk.Version} ({Toolchain.PlatformIdentifier()}) at {sdk.Root}...");
        ValidateBackend(installation, installed, sdk, OperatingSystem.IsWindows());
        if (OperatingSystem.IsWindows())
        {
            var product = installation.Resources.Single(resource => resource.Kind == InstalledResourceKind.WindowsInstaller && resource.Detail == installation.CliVersion);
            if (!WindowsInstallation.IsInstalled(product.Target))
                throw new InvalidOperationException("Windows Installer no longer records this SDK. Run isoplane setup to repair the MSI installation.");
        }
        if (OperatingSystem.IsLinux())
        {
            var missing = Prerequisites.Missing(Prerequisites.CanLoad, File.Exists);
            if (missing.Count != 0)
                throw new InvalidOperationException($"Missing native prerequisites: {Prerequisites.Describe(missing)}. Run isoplane setup to install them.");
        }
        Console.Error.WriteLine("Checking native tools and compiler dependencies...");
        await Readiness.ProveToolsAsync(installed, cancellation);
        Console.WriteLine("Your Isoplane toolchain is ready.");
        return 0;
    }

    internal static void ValidateBackend(Installation installation, string installed, WorkloadSdk sdk, bool windows)
    {
        if (windows)
        {
            if (installation.Resources.Count(resource => resource.Kind == InstalledResourceKind.WindowsInstaller && resource.Detail == installation.CliVersion) != 1
                || !File.Exists(Path.Combine(installed, "Sdk", "Sdk.props")))
                throw new InvalidOperationException("The MSI SDK registration or build props are missing. Run isoplane setup to repair the installation.");
            return;
        }
        var manifest = Path.Combine(sdk.Root, "sdk-manifests", sdk.FeatureBand, WorkloadInstallation.Id, installation.CliVersion);
        var record = Path.Combine(sdk.Root, "metadata", "workloads", sdk.FeatureBand, "InstalledWorkloads", WorkloadInstallation.Id);
        var pack = Path.Combine(sdk.Root, "packs", "Isoplane.Toolchain." + Toolchain.PlatformIdentifier(), installation.CliVersion);
        if (Path.GetFullPath(installed) != Path.GetFullPath(pack) || !File.Exists(record)
            || !File.Exists(Path.Combine(manifest, "WorkloadManifest.targets"))
            || !File.Exists(Path.Combine(manifest, "WorkloadManifest.json"))
            || !File.Exists(Path.Combine(pack, "Sdk", "Sdk.props")))
            throw new InvalidOperationException($"Isoplane is not fully registered for the selected .NET SDK {sdk.Version} (feature band {sdk.FeatureBand}). Run isoplane setup with this SDK selected, then retry.");
        try
        {
            using var description = JsonDocument.Parse(File.ReadAllBytes(Path.Combine(manifest, "WorkloadManifest.json")));
            if (description.RootElement.ValueKind != JsonValueKind.Object
                || !description.RootElement.TryGetProperty("version", out var version)
                || version.ValueKind != JsonValueKind.String || version.GetString() != installation.CliVersion)
                throw new InvalidOperationException("The Isoplane workload manifest version differs from the installed CLI. Run isoplane setup to repair it.");
            ValidatePacks(description.RootElement, sdk);
        }
        catch (Exception error) when (error is JsonException or IOException or UnauthorizedAccessException)
        {
            throw new InvalidOperationException("The Isoplane workload manifest is unreadable. Run isoplane setup to repair it.", error);
        }
    }

    private static void ValidatePacks(JsonElement manifest, WorkloadSdk sdk)
    {
        if (!manifest.TryGetProperty("packs", out var packs) || packs.ValueKind != JsonValueKind.Object || !packs.EnumerateObject().Any())
            throw new InvalidOperationException("The Isoplane workload manifest has no pack definitions. Run isoplane setup to repair it.");
        foreach (var pack in packs.EnumerateObject())
        {
            var definition = pack.Value;
            if (definition.ValueKind != JsonValueKind.Object || !definition.TryGetProperty("kind", out var kind))
                throw new InvalidOperationException("The Isoplane workload manifest has an invalid pack definition. Run isoplane setup to repair it.");
            if (kind.ValueKind != JsonValueKind.String || kind.GetString() is not ("sdk" or "template"))
                throw new InvalidOperationException("The Isoplane workload manifest has an unsupported pack kind. Run isoplane setup to repair it.");
            if (kind.GetString() == "template") continue;
            var name = pack.Name;
            if (definition.TryGetProperty("alias-to", out var aliases))
            {
                if (aliases.ValueKind != JsonValueKind.Object || !aliases.TryGetProperty(Toolchain.PlatformIdentifier(), out var alias)
                    || alias.ValueKind != JsonValueKind.String || string.IsNullOrWhiteSpace(alias.GetString()))
                    throw new InvalidOperationException($"The Isoplane workload has no compatible {name} pack. Run isoplane setup to repair it.");
                name = alias.GetString()!;
            }
            if (!definition.TryGetProperty("version", out var version) || version.ValueKind != JsonValueKind.String
                || string.IsNullOrWhiteSpace(version.GetString())
                || !File.Exists(Path.Combine(sdk.Root, "packs", name, version.GetString()!, "Sdk", "Sdk.props")))
                throw new InvalidOperationException($"The {name} SDK pack is missing or incomplete. Run isoplane setup to repair it.");
        }
    }
}
