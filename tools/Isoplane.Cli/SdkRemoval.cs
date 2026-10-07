namespace Isoplane.Cli;

/// <summary>
/// <c>isoplane uninstall-sdk</c>: removes what the installation manifest records setup owning, registrations before
/// their package files, and keeps the manifest with whatever could not be removed so the command can be repeated.
/// </summary>
internal static class SdkRemoval
{
    internal static async Task<int> RunAsync(string home, Func<string, ISdkRegistrations> registrations, CancellationToken cancellation)
    {
        home = Path.GetFullPath(home);
        var installation = Installation.Load(home);
        if (installation is null)
        {
            Console.Error.WriteLine($"isoplane: no installation manifest at {Installation.PathIn(home)}; nothing recorded by isoplane setup remains to remove. An SDK installed another way must be removed by hand.");
            return 1;
        }
        using var held = new FileStream(Path.Combine(home, "setup.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
        var dotnet = new Lazy<ISdkRegistrations>(() => registrations(DotnetRegistrations.BootstrapDotnet()));
        var removed = 0;
        var kept = 0;
        var failed = new List<InstalledResource>();
        foreach (var resource in RemovalOrder(installation.Resources))
        {
            if (!resource.Owned)
            {
                Console.Error.WriteLine($"Kept {Describe(resource)}: it existed before isoplane setup.");
                installation.Forget(resource);
                kept++;
                continue;
            }
            try
            {
                await RemoveAsync(resource, dotnet, cancellation);
                installation.Forget(resource);
                removed++;
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidOperationException)
            {
                Console.Error.WriteLine($"isoplane: could not remove {Describe(resource)}: {error.Message}");
                installation.Record(resource with { RemovalFailure = error.Message });
                failed.Add(resource);
            }
        }
        if (failed.Count > 0)
        {
            Console.Error.WriteLine($"isoplane: {failed.Count} of {removed + failed.Count} recorded items could not be removed; they remain listed in {installation.Path}. Resolve the failures and run isoplane uninstall-sdk again.");
            return 1;
        }
        installation.Delete();
        Console.WriteLine($"Removed the Isoplane SDK: {removed} items removed{(kept > 0 ? $", {kept} pre-existing items kept" : "")}.");
        Console.WriteLine("The isoplane CLI remains installed; run isoplane setup to install the SDK again, or dotnet tool uninstall --global Isoplane.Cli to remove the CLI.");
        return 0;
    }

    /// <summary>
    /// Remove registrations before their backing files.
    /// </summary>
    private static InstalledResource[] RemovalOrder(IReadOnlyList<InstalledResource> resources) =>
        resources.Select((resource, index) => (resource, index))
            .OrderBy(entry => entry.resource.Kind switch
            {
                InstalledResourceKind.Template => -3,
                InstalledResourceKind.PackageSource => 1,
                InstalledResourceKind.File => 3,
                InstalledResourceKind.Directory => 5,
                _ => throw new InvalidOperationException($"unknown resource kind {entry.resource.Kind}"),
            })
            .ThenByDescending(entry => entry.index)
            .Select(entry => entry.resource)
            .ToArray();

    private static async Task RemoveAsync(InstalledResource resource, Lazy<ISdkRegistrations> dotnet, CancellationToken cancellation)
    {
        switch (resource.Kind)
        {
            case InstalledResourceKind.Directory:
                if (Directory.Exists(resource.Target))
                    Directory.Delete(resource.Target, recursive: true);
                break;
            case InstalledResourceKind.File:
                File.Delete(resource.Target);
                break;
            case InstalledResourceKind.PackageSource:
                if (await dotnet.Value.PackageSourceAsync(resource.Target, cancellation) is not null)
                    await dotnet.Value.RemovePackageSourceAsync(resource.Target, cancellation);
                break;
            case InstalledResourceKind.Template:
                if (await dotnet.Value.TemplateVersionAsync(resource.Target, cancellation) is not null)
                    await dotnet.Value.UninstallTemplateAsync(resource.Target, cancellation);
                break;
        }
    }

    private static string Describe(InstalledResource resource) => resource.Kind switch
    {
        InstalledResourceKind.PackageSource => $"NuGet package source '{resource.Target}'",
        InstalledResourceKind.Template => $"template package {resource.Target}",
        InstalledResourceKind.File or InstalledResourceKind.Directory => resource.Target,
        _ => throw new InvalidOperationException($"unknown resource kind {resource.Kind}"),
    };
}
