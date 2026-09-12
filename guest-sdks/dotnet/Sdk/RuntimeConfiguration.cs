using System;
using System.Collections.Generic;
using System.Text.Json;

namespace Wisp;

/// <summary>Application settings supplied by the host after template activation.</summary>
public static class RuntimeConfiguration
{
    /// <summary>Reads settings for this application. Refuses access during preparation so secrets cannot enter its template.</summary>
    public static IReadOnlyDictionary<string, string> ReadValues()
    {
        if (!Activation.IsActivated)
            throw new InvalidOperationException("Runtime configuration is available after activation; resolve settings when handling requests or starting hosted services.");
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
