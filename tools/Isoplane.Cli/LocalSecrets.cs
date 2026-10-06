using Microsoft.Extensions.Configuration;

namespace Isoplane.Cli;

internal static class LocalSecrets
{
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
