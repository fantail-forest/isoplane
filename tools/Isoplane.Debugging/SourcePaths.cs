using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Newtonsoft.Json.Linq;

namespace Isoplane.Debugging;

internal sealed class SourcePaths
{
    private KeyValuePair<string, string>[] mappings = [];

    internal void Configure(JToken? value)
    {
        if (value is null) { mappings = []; return; }
        if (value is not JObject entries)
            throw new ProtocolException("sourceFileMap must map build source directories to absolute local directories.");
        var parsed = new List<KeyValuePair<string, string>>();
        foreach (var entry in entries.Properties())
        {
            var remote = entry.Name.Replace('\\', '/');
            if (remote != "/" && !(remote.Length == 3 && remote[1..] == ":/")) remote = remote.TrimEnd('/');
            var local = entry.Value.Type == JTokenType.String ? entry.Value.Value<string>() : null;
            if (remote.Length == 0 || !(remote.StartsWith('/') || remote.Length > 2 && char.IsAsciiLetter(remote[0]) && remote[1..3] == ":/")
                || string.IsNullOrWhiteSpace(local) || !Path.IsPathFullyQualified(local))
                throw new ProtocolException("sourceFileMap requires absolute build and local source directories.");
            parsed.Add(new(remote, Path.GetFullPath(local)));
        }
        mappings = parsed.OrderByDescending(pair => pair.Key.Length).ToArray();
    }

    internal string ToClient(string path)
    {
        var normalized = path.Replace('\\', '/');
        foreach (var (remote, local) in mappings)
        {
            var comparison = remote.Length > 1 && remote[1] == ':' ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
            if (normalized.Equals(remote, comparison)) return local;
            var prefix = remote.EndsWith('/') ? remote : remote + "/";
            if (normalized.StartsWith(prefix, comparison))
                return Path.GetFullPath(Path.Combine(local, normalized[prefix.Length..].Replace('/', Path.DirectorySeparatorChar)));
        }
        return Path.IsPathFullyQualified(path) ? Path.GetFullPath(path) : path;
    }
}
