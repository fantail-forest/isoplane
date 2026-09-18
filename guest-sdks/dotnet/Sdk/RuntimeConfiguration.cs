using System;
using System.Collections.Generic;
using System.Text.Json;

namespace Isoplane;

/// <summary>Deployment-bound application settings supplied during preparation and activation.</summary>
public static class RuntimeConfiguration
{
    /// <summary>Reads settings for this deployment, including during template initialization.</summary>
    public static IReadOnlyDictionary<string, string> ReadValues()
    {
        return ParseValues(Abi.RuntimeConfigurationInterop.Read());
    }

    internal static IReadOnlyDictionary<string, string> ParseValues(ReadOnlyMemory<byte> bytes)
    {
        using var document = JsonDocument.Parse(bytes);
        var values = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (var property in document.RootElement.EnumerateObject())
            if (!values.TryAdd(property.Name, property.Value.GetString()
                ?? throw new JsonException("Runtime configuration values must be strings.")))
                throw new JsonException("Runtime configuration keys must be unique ignoring case.");
        return values;
    }
}
