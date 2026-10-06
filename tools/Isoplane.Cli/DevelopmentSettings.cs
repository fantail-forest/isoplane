using System.Text.Json;

namespace Isoplane.Cli;

/// <summary>
/// What a development session runs an application with: its name, listener,
/// configuration, limits and trusted roots. Nothing here reaches a deployment.
/// </summary>
internal sealed record DevelopmentSettings(string Name, ushort? Port, Dictionary<string, string> Configuration, LimitOverrides Limits, TrustedRoots? Trust)
{
    /// <summary>The prefix of environment variables that supply application settings to a session.</summary>
    internal const string SettingPrefix = "ISOPLANE_APP_";

    /// <summary>
    /// Evaluates the project and its launch profile. User Secrets supply
    /// configuration; the profile's environment variables override them, as
    /// environment variables override User Secrets in ASP.NET. This process's
    /// own <see cref="SettingPrefix"/> variables override both, so a test or
    /// a shell can supply a setting without editing the project.
    /// </summary>
    internal static async Task<DevelopmentSettings> ResolveAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var project = await ProjectProperties.ReadAsync(options, cancellation);
        var profile = options.UseLaunchProfile ? LaunchProfile.Read(options.Directory, options.LaunchProfile) : null;
        var configuration = Layer(LocalSecrets.Read(project.UserSecretsId), profile, Environment.GetEnvironmentVariables());
        return new(project.AssemblyName, options.Port ?? profile?.Port, configuration, profile?.Limits ?? LimitOverrides.None,
            MachineTrust.Export(new LocalProject(options.Directory).Trust));
    }

    /// <summary>Applies the launch profile's variables, then this process's prefixed ones, over <paramref name="secrets"/>.</summary>
    internal static Dictionary<string, string> Layer(Dictionary<string, string> secrets, LaunchProfile? profile, System.Collections.IDictionary environment)
    {
        static string Key(string variable) => variable.Replace("__", ":", StringComparison.Ordinal);
        if (profile is not null)
            foreach (var variable in profile.Environment)
                secrets[Key(variable.Key)] = variable.Value;
        foreach (System.Collections.DictionaryEntry variable in environment)
            if (variable.Key is string name && name.StartsWith(SettingPrefix, StringComparison.OrdinalIgnoreCase) && name.Length > SettingPrefix.Length)
                secrets[Key(name[SettingPrefix.Length..])] = (string?)variable.Value ?? "";
        return secrets;
    }
}

/// <summary>The evaluated MSBuild properties a development session needs before it builds.</summary>
internal sealed record ProjectProperties(string AssemblyName, string UserSecretsId)
{
    internal static async Task<ProjectProperties> ReadAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var project = Directory.GetFiles(options.Directory, "*.csproj").Single();
        using var process = Processes.Start("dotnet", ["msbuild", project, "-p:Configuration=" + options.Configuration,
            "-getProperty:AssemblyName,UserSecretsId"], options.Directory, captureOutput: true);
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
                properties.GetProperty("UserSecretsId").GetString() ?? "");
        }
        finally
        {
            await Processes.StopAsync(process);
        }
    }
}
