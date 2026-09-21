using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.Versioning;
using System.Security.Principal;
using System.Text.Json;
using Microsoft.Win32;

namespace Isoplane.Cli;

/// <summary>Installs SDK files through Windows Installer without registering .NET workloads.</summary>
[SupportedOSPlatform("windows")]
internal sealed class WindowsInstallation : ISdkInstallation
{
    private sealed record Package(string Version, string ProductCode);

    public async Task<string> InstallAsync(string bundle, string version, Installation installation, CancellationToken cancellation)
    {
        var package = JsonSerializer.Deserialize<Package>(await File.ReadAllTextAsync(Path.Combine(bundle, "WindowsInstaller.json"), cancellation))
            ?? throw new InvalidOperationException("The Windows installer descriptor is missing.");
        if (package.Version != version || !Guid.TryParse(package.ProductCode, out var id))
            throw new InvalidOperationException("The Windows installer does not match this release. Download a complete matching release and retry setup.");
        var productCode = id.ToString("B").ToUpperInvariant();
        var directory = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "Isoplane", "SDK", version);
        var previous = installation.Find(InstalledResourceKind.WindowsInstaller, productCode);
        var installed = IsInstalled(productCode);
        if (!installed && Directory.Exists(directory) && previous is not { Owned: true })
            throw new InvalidOperationException($"Setup will not replace the unowned directory {directory}. Move it aside and retry.");
        var owned = previous?.Owned ?? !installed;
        await InstallRuntimeAsync(bundle, cancellation);
        if (!installed || !await IsHealthyAsync(directory, cancellation))
        {
            if (!owned)
                throw new InvalidOperationException("This SDK was installed outside isoplane setup and needs repair. Repair it from Windows Installed apps, then retry setup.");
            Console.Error.WriteLine(installed ? "Repairing the Isoplane SDK with Windows Installer..." : "Installing the Isoplane SDK with Windows Installer...");
            installation.Record(new(InstalledResourceKind.WindowsInstaller, productCode, true, version));
            await RunInstallerAsync("msiexec.exe", ["/i", Path.Combine(bundle, "sdk.msi"), "REINSTALLMODE=vomus", .. installed ? new[] { "REINSTALL=ALL" } : [], "/passive", "/norestart"], cancellation);
            if (!IsInstalled(productCode) || !await IsHealthyAsync(directory, cancellation))
                throw new InvalidOperationException("Windows Installer finished without a healthy SDK installation. Rerun isoplane setup to repair it.");
        }
        installation.Record(new(InstalledResourceKind.WindowsInstaller, productCode, owned, version));
        var dotnet = DotnetRegistrations.BootstrapDotnet();
        var registrations = new DotnetRegistrations(dotnet, Path.GetTempPath());
        var existing = await registrations.TemplateVersionAsync("Isoplane.Templates", cancellation);
        var template = installation.Find(InstalledResourceKind.Template, "Isoplane.Templates");
        if (existing != version)
        {
            if (existing is not null && template is not { Owned: true })
                throw new InvalidOperationException("An Isoplane template installed outside setup already exists at another version. Remove it with dotnet new uninstall Isoplane.Templates, then retry setup.");
            installation.Record(new(InstalledResourceKind.Template, "Isoplane.Templates", true, version));
            await Processes.CheckAsync(dotnet, ["new", "install", Path.Combine(directory, "templates", $"Isoplane.Templates.{version}.nupkg"), "--force"], Path.GetTempPath(), cancellation);
        }
        else
            installation.Record(new(InstalledResourceKind.Template, "Isoplane.Templates", template?.Owned ?? false, version));
        return directory;
    }

    public async Task RemoveAsync(InstalledResource resource, CancellationToken cancellation)
    {
        if (resource.Kind != InstalledResourceKind.WindowsInstaller || !Guid.TryParse(resource.Target, out var id))
            throw new InvalidOperationException("The installation record does not identify an Isoplane Windows Installer product.");
        var productCode = id.ToString("B").ToUpperInvariant();
        if (IsInstalled(productCode))
            await RunInstallerAsync("msiexec.exe", ["/x", productCode, "/passive", "/norestart"], cancellation);
    }

    private static bool IsInstalled(string productCode)
    {
        using var key = Registry.LocalMachine.OpenSubKey(@"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\" + productCode);
        return key is not null;
    }

    private static async Task<bool> IsHealthyAsync(string directory, CancellationToken cancellation)
    {
        try
        {
            var manifest = Toolchain.ReadManifest(await File.ReadAllBytesAsync(Path.Combine(directory, Toolchain.ManifestName), cancellation));
            await Toolchain.VerifyAsync(directory, manifest, cancellation);
            return true;
        }
        catch (Exception error) when (error is IOException or InvalidOperationException or JsonException)
        {
            return false;
        }
    }

    private static async Task InstallRuntimeAsync(string bundle, CancellationToken cancellation)
    {
        if (new[] { "msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll" }.All(Prerequisites.CanLoad))
            return;
        Console.Error.WriteLine("Installing the Microsoft Visual C++ runtime required by the WebAssembly tools...");
        await RunInstallerAsync(Path.Combine(bundle, "vc_redist.x64.exe"), ["/install", "/passive", "/norestart"], cancellation);
    }

    private static async Task RunInstallerAsync(string executable, string[] arguments, CancellationToken cancellation)
    {
        cancellation.ThrowIfCancellationRequested();
        using var identity = WindowsIdentity.GetCurrent();
        var administrator = new WindowsPrincipal(identity).IsInRole(WindowsBuiltInRole.Administrator);
        var start = new ProcessStartInfo(executable) { UseShellExecute = true, Verb = administrator ? "" : "runas" };
        foreach (var argument in arguments)
            start.ArgumentList.Add(argument);
        Process started;
        try
        {
            started = Process.Start(start) ?? throw new InvalidOperationException($"Cannot start {executable}.");
        }
        catch (Win32Exception error)
        {
            throw new InvalidOperationException($"Windows could not start the installer ({error.Message}). Authorize administrator access when prompted, then retry the Isoplane command.", error);
        }
        using var process = started;
        // Windows Installer must finish or roll back its transaction before setup releases its lock.
        await process.WaitForExitAsync();
        if (process.ExitCode is not (0 or 3010))
            throw new InvalidOperationException($"Windows installation failed (exit {process.ExitCode}). Resolve the installer error and rerun the Isoplane command.");
        if (process.ExitCode == 3010)
            Console.Error.WriteLine("Windows reports that a restart is required to finish installation.");
    }
}
