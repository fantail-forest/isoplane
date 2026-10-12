using System;
using System.Collections.Generic;
using System.Text.Json;

namespace Isoplane;

/// <summary>
/// The settings Isoplane supplies to the application, such as connection strings and credentials,
/// for an application written without ASP.NET.
/// </summary>
/// <remarks>
/// <para>
/// An ASP.NET application does not need this type. <c>IsoplaneHosting.CreateBuilder</c> adds the
/// same settings to <c>builder.Configuration</c>, so read them through <c>IConfiguration</c>.
/// </para>
/// <para>
/// In local development the settings come from the project's User Secrets, the
/// <c>environmentVariables</c> of its launch profile, and environment variables named
/// <c>ISOPLANE_APP_&lt;Key&gt;</c>, each overriding the one before.
/// <see href="https://isoplane.dev/docs/local-settings">Supply local development settings</see>
/// describes each source. The application sees none of the machine's other environment variables.
/// </para>
/// </remarks>
public static class RuntimeConfiguration
{
    /// <summary>Reads the application's settings.</summary>
    /// <returns>
    /// The settings by name. Names use ASP.NET's form, <c>Section:Key</c>, and are matched ignoring
    /// case. The dictionary is empty when the application has no settings.
    /// </returns>
    /// <remarks>
    /// It can be called while the application starts and while it handles a request. A value read in
    /// an <see cref="InitializerAttribute"/> method and kept in a static field is saved in the
    /// template and shared by every instance.
    /// </remarks>
    /// <example>
    /// <code>
    /// var settings = RuntimeConfiguration.ReadValues();
    /// var greeting = settings.GetValueOrDefault("Greeting:Default") ?? "world";
    /// </code>
    /// </example>
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
