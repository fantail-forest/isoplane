namespace Isoplane.Cli;

internal static class EnvironmentDoctor
{
    internal static async Task<int> RunAsync(CancellationToken cancellation)
    {
        // Each check is named before it runs. Two of them start .NET commands that take seconds,
        // and a failure then follows the name of what was being checked.
        Console.Error.WriteLine("Checking the .NET SDK and the native host library...");
        await Readiness.ProveAsync(cancellation);
        if (OperatingSystem.IsLinux())
        {
            var missing = Prerequisites.Missing(Prerequisites.CanLoad, File.Exists);
            if (missing.Count != 0)
                throw new InvalidOperationException($"Missing native prerequisites: {Prerequisites.Describe(missing)}. Run isoplane setup.");
        }
        Console.Error.WriteLine("Checking project templates...");
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
