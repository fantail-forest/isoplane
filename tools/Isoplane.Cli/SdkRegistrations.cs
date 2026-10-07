using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text.RegularExpressions;

namespace Isoplane.Cli;

/// <summary>
/// The registrations a .NET SDK keeps for its user: NuGet package sources and template packages.
/// Setup registers templates and, for an explicit offline source, a package feed.
/// </summary>
internal interface ISdkRegistrations
{
    /// <summary>The path of the named package source, or null when none is configured.</summary>
    Task<string?> PackageSourceAsync(string name, CancellationToken cancellation);
    Task AddPackageSourceAsync(string name, string path, CancellationToken cancellation);
    Task RemovePackageSourceAsync(string name, CancellationToken cancellation);
    /// <summary>The version of the registered template package, or null when it is not registered.</summary>
    Task<string?> TemplateVersionAsync(string packageId, CancellationToken cancellation);
    /// <summary>Installs a template package from an ID::version or a local package file.</summary>
    Task InstallTemplateAsync(string source, CancellationToken cancellation);
    Task UninstallTemplateAsync(string packageId, CancellationToken cancellation);
}

/// <summary>Registrations made through a <c>dotnet</c> executable, run from a directory with no project configuration.</summary>
internal sealed partial class DotnetRegistrations(string dotnet, string workingDirectory) : ISdkRegistrations
{
    public async Task<string?> PackageSourceAsync(string name, CancellationToken cancellation) =>
        SourcePath(await RunAsync(["nuget", "list", "source"], cancellation), name);

    /// <summary>The path of the named source in <c>dotnet nuget list source</c> output: each entry is a numbered name line followed by its path.</summary>
    internal static string? SourcePath(string listing, string name)
    {
        string? current = null;
        foreach (var line in listing.Split('\n').Select(line => line.Trim()))
        {
            var entry = SourceEntry().Match(line);
            if (entry.Success)
            {
                current = entry.Groups["name"].Value;
                continue;
            }
            if (current == name && line.Length > 0)
                return line;
        }
        return null;
    }

    public Task AddPackageSourceAsync(string name, string path, CancellationToken cancellation) =>
        RunAsync(["nuget", "add", "source", path, "--name", name], cancellation);

    public Task RemovePackageSourceAsync(string name, CancellationToken cancellation) =>
        RunAsync(["nuget", "remove", "source", name], cancellation);

    public async Task<string?> TemplateVersionAsync(string packageId, CancellationToken cancellation) =>
        TemplateVersion(await RunAsync(["new", "uninstall"], cancellation), packageId);

    /// <summary>The version of a package in <c>dotnet new uninstall</c> output: the package id on its own line, then its indented details.</summary>
    internal static string? TemplateVersion(string listing, string packageId)
    {
        var lines = listing.Split('\n').Select(line => line.Trim()).ToArray();
        for (var index = 0; index < lines.Length; index++)
        {
            if (lines[index] != packageId)
                continue;
            for (var detail = index + 1; detail < lines.Length && lines[detail].Length > 0; detail++)
                if (lines[detail].StartsWith("Version:", StringComparison.Ordinal))
                    return lines[detail]["Version:".Length..].Trim();
            return "";
        }
        return null;
    }

    public Task InstallTemplateAsync(string source, CancellationToken cancellation) =>
        RunAsync(["new", "install", source, "--force"], cancellation);

    public Task UninstallTemplateAsync(string packageId, CancellationToken cancellation) =>
        RunAsync(["new", "uninstall", packageId], cancellation);

    internal Task<string> SdkVersionAsync(CancellationToken cancellation) => RunAsync(["--version"], cancellation);

    private async Task<string> RunAsync(string[] arguments, CancellationToken cancellation)
    {
        var start = new ProcessStartInfo(dotnet) { WorkingDirectory = workingDirectory, UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var argument in arguments)
            start.ArgumentList.Add(argument);
        start.Environment["DOTNET_CLI_TELEMETRY_OPTOUT"] = "1";
        start.Environment["DOTNET_NOLOGO"] = "1";
        start.Environment["DOTNET_SKIP_FIRST_TIME_EXPERIENCE"] = "1";
        using var process = Process.Start(start) ?? throw new InvalidOperationException($"Cannot start {dotnet}.");
        var output = process.StandardOutput.ReadToEndAsync(cancellation);
        var errors = process.StandardError.ReadToEndAsync(cancellation);
        try
        {
            await process.WaitForExitAsync(cancellation);
        }
        catch (OperationCanceledException)
        {
            process.Kill(entireProcessTree: true);
            throw;
        }
        var text = await output;
        if (process.ExitCode != 0)
            throw new InvalidOperationException($"dotnet {string.Join(' ', arguments)} failed (exit {process.ExitCode}):{Environment.NewLine}{(text + await errors).Trim()}");
        return text;
    }

    [GeneratedRegex(@"^\d+\.\s+(?<name>.+?)\s+\[(Enabled|Disabled)\]$")]
    private static partial Regex SourceEntry();

    /// <summary>
    /// Finds the <c>dotnet</c> hosting this CLI, falling back to the executable search path.
    /// </summary>
    internal static string BootstrapDotnet()
    {
        var executable = OperatingSystem.IsWindows() ? "dotnet.exe" : "dotnet";
        var runtime = Path.GetFullPath(RuntimeEnvironment.GetRuntimeDirectory());
        for (var directory = Path.GetDirectoryName(runtime.TrimEnd(Path.DirectorySeparatorChar)); directory is not null; directory = Path.GetDirectoryName(directory))
        {
            var candidate = Path.Combine(directory, executable);
            if (File.Exists(candidate))
                return candidate;
        }
        foreach (var directory in (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries))
        {
            var candidate = Path.Combine(directory, executable);
            if (File.Exists(candidate))
                return candidate;
        }
        throw new InvalidOperationException("No dotnet executable is available to register or unregister SDK components; install a .NET SDK and retry.");
    }
}
