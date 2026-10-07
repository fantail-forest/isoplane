namespace Isoplane.Cli;

internal static class EnvironmentDoctor
{
    internal static async Task<int> RunAsync(CancellationToken cancellation)
    {
        await Readiness.ProveAsync(cancellation);
        if (OperatingSystem.IsLinux())
        {
            var missing = Prerequisites.Missing(Prerequisites.CanLoad, File.Exists);
            if (missing.Count != 0)
                throw new InvalidOperationException($"Missing native prerequisites: {Prerequisites.Describe(missing)}. Run isoplane setup.");
        }
        var registrations = new DotnetRegistrations(DotnetRegistrations.BootstrapDotnet(), Environment.CurrentDirectory);
        var version = ReleaseIdentity.OfRunningCli().Version;
        RequireTemplates(await registrations.TemplateVersionAsync("Isoplane.Templates", cancellation), version);
        Console.WriteLine("Your Isoplane SDK is ready.");
        return 0;
    }

    internal static void RequireTemplates(string? installed, string expected)
    {
        if (installed != expected)
            throw new InvalidOperationException($"Isoplane.Templates {expected} is not registered. Run isoplane setup.");
    }
}
