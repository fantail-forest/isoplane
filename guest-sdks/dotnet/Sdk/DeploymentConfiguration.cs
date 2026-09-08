using System;
using System.Collections.Generic;
using System.Text.Json;
using Wisp.Abi;

namespace Wisp;

/// <summary>Non-secret settings fixed for an application version at deployment.</summary>
public static class DeploymentConfiguration
{
    /// <summary>
    /// Reads the manifest's <c>init_visible_vars</c>, preserving order and duplicate keys.
    /// Call during pre-initialization and retain the result for request handling;
    /// the host refuses this import after activation. These values become part of
    /// the template and must not contain runtime secrets.
    /// </summary>
    public static IReadOnlyList<KeyValuePair<string, string>> ReadVariables()
    {
        byte[] data = Initialization.ReadConfiguration();
        if (data.Length == 0)
            return Array.Empty<KeyValuePair<string, string>>();
        using JsonDocument document = JsonDocument.Parse(data);
        var variables = new List<KeyValuePair<string, string>>();
        foreach (JsonElement pair in document.RootElement.EnumerateArray())
        {
            if (pair.GetArrayLength() != 2)
                throw new JsonException("A deployment variable must contain a name and value.");
            string name = pair[0].GetString() ?? throw new JsonException("A deployment variable name cannot be null.");
            string value = pair[1].GetString() ?? throw new JsonException("A deployment variable value cannot be null.");
            variables.Add(new KeyValuePair<string, string>(name, value));
        }
        return variables.AsReadOnly();
    }
}
