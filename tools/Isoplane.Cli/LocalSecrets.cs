using Microsoft.Extensions.Configuration;

namespace Isoplane.Cli;

internal static class LocalSecrets
{
    internal static async Task<Dictionary<string, string>> ReadAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        var project = Directory.GetFiles(options.Directory, "*.csproj").Single();
        using var process = Processes.Start("dotnet", ["msbuild", project, "-p:Configuration=" + options.Configuration,
            "-getProperty:UserSecretsId"], options.Directory, captureOutput: true);
        try
        {
            var id = (await process.StandardOutput.ReadToEndAsync(cancellation)).Trim();
            await process.WaitForExitAsync(cancellation);
            if (process.ExitCode != 0)
                throw new InvalidOperationException("Could not evaluate UserSecretsId; fix the project diagnostics above.");
            return Read(id);
        }
        finally
        {
            await Processes.StopAsync(process);
        }
    }

    internal static Dictionary<string, string> Read(string id)
    {
        if (string.IsNullOrWhiteSpace(id))
            return new(StringComparer.OrdinalIgnoreCase);
        using var configuration = new ConfigurationManager();
        configuration.AddUserSecrets(id, reloadOnChange: false);
        return configuration.AsEnumerable().Where(pair => pair.Value is not null)
            .ToDictionary(pair => pair.Key, pair => pair.Value!, StringComparer.OrdinalIgnoreCase);
    }
}
