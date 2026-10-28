namespace Isoplane.Cli;

internal static class EnvironmentDoctor
{
    internal static async Task<int> RunAsync(CancellationToken cancellation)
    {
        Console.WriteLine(await CheckAsync(Console.Error, cancellation));
        return 0;
    }

    /// <summary>
    /// Runs every check, naming each on <paramref name="progress"/> before it runs, and returns the
    /// ready message. A failed check throws <see cref="InvalidOperationException"/> saying what to do.
    /// </summary>
    internal static async Task<string> CheckAsync(TextWriter progress, CancellationToken cancellation)
    {
        // Each check is named before it runs. Two of them start .NET commands that take seconds,
        // and a failure then follows the name of what was being checked.
        progress.WriteLine("Checking the .NET SDK and the native host library...");
        await Readiness.ProveAsync(cancellation);
        if (OperatingSystem.IsLinux())
        {
            var missing = Prerequisites.Missing(Prerequisites.CanLoad, Prerequisites.HasTrustedRoots);
            if (missing.Count != 0)
                throw new InvalidOperationException($"Missing native prerequisites: {Prerequisites.Describe(missing)}. Run isoplane setup.");
        }
        progress.WriteLine("Checking project templates...");
        var registrations = new DotnetRegistrations(DotnetRegistrations.BootstrapDotnet(), Environment.CurrentDirectory);
        var version = ReleaseIdentity.OfRunningCli().Version;
        RequireTemplates(await registrations.TemplateVersionAsync("Isoplane.Templates", cancellation), version);
        return $"Your Isoplane SDK {version} is ready.";
    }

    internal static void RequireTemplates(string? installed, string expected)
    {
        if (installed != expected)
            throw new InvalidOperationException($"Isoplane.Templates {expected} is not registered. Run isoplane setup.");
    }
}
