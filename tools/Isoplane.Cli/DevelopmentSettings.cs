using System.Text.Json;

namespace Isoplane.Cli;

/// <summary>
/// What a development session runs an application with: its name, listener,
/// configuration, limits and trusted roots, and where it keeps what it prepares.
/// Nothing here reaches a deployment.
/// </summary>
/// <param name="IntermediateDirectory">The project's MSBuild intermediate output directory, for what the session prepares from this project's build.</param>
/// <param name="RuntimeCacheDirectory">Where the compiled shared runtime is kept for every project that uses it.</param>
internal sealed record DevelopmentSettings(string Name, ushort? Port, Dictionary<string, string> Configuration, LimitOverrides Limits, string? MachineRoots,
    string IntermediateDirectory, string RuntimeCacheDirectory)
{
    /// <summary>
    /// Evaluates the project and its launch profile. User Secrets supply
    /// configuration; the profile's environment variables override them, as
    /// environment variables override User Secrets in ASP.NET; the session's
    /// <c>--settings</c> override both.
    /// </summary>
    internal static async Task<DevelopmentSettings> ResolveAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var project = await ProjectProperties.ReadAsync(options, cancellation);
        var profile = options.UseLaunchProfile ? LaunchProfile.Read(options.Directory, options.LaunchProfile) : null;
        var configuration = Layer(LocalSecrets.Read(project.UserSecretsId), profile, options.Settings);
        return new(project.AssemblyName, options.Port ?? profile?.Port, configuration, profile?.Limits ?? LimitOverrides.None,
            MachineTrust.Export(new LocalProject(options.Directory).Trust), project.IntermediateDirectory, CliPaths.RuntimeCache);
    }

    /// <summary>Applies the launch profile's variables, then the session's <paramref name="settings"/>, over <paramref name="secrets"/>.</summary>
    internal static Dictionary<string, string> Layer(Dictionary<string, string> secrets, LaunchProfile? profile, IReadOnlyDictionary<string, string> settings)
    {
        if (profile is not null)
            foreach (var variable in profile.Environment)
                secrets[variable.Key.Replace("__", ":", StringComparison.Ordinal)] = variable.Value;
        foreach (var setting in settings)
            secrets[setting.Key] = setting.Value;
        return secrets;
    }
}

/// <summary>The evaluated MSBuild properties a development session needs before it builds.</summary>
internal sealed record ProjectProperties(string AssemblyName, string UserSecretsId, string IntermediateDirectory)
{
    internal static async Task<ProjectProperties> ReadAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var project = Directory.GetFiles(options.Directory, "*.csproj").Single();
        using var process = Processes.Start("dotnet", ["msbuild", project, "-p:Configuration=" + options.Configuration,
            "-getProperty:AssemblyName,UserSecretsId,IntermediateOutputPath"], options.Directory, captureOutput: true);
        try
        {
            var output = await process.StandardOutput.ReadToEndAsync(cancellation);
            await process.WaitForExitAsync(cancellation);
            if (process.ExitCode != 0)
                throw new InvalidOperationException("Could not evaluate the project; fix the project diagnostics above.");
            using var evaluation = JsonDocument.Parse(output);
            var properties = evaluation.RootElement.GetProperty("Properties");
            return new(properties.GetProperty("AssemblyName").GetString() is { Length: > 0 } name ? name
                : throw new InvalidOperationException("The project evaluates to no assembly name."),
                properties.GetProperty("UserSecretsId").GetString() ?? "",
                properties.GetProperty("IntermediateOutputPath").GetString() is { Length: > 0 } intermediate ? Path.GetFullPath(intermediate, Path.GetFullPath(options.Directory))
                : throw new InvalidOperationException("The project evaluates to no intermediate output path."));
        }
        finally
        {
            await Processes.StopAsync(process);
        }
    }
}
