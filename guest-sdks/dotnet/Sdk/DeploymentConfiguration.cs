using System;
using System.Collections.Generic;
using System.Text.Json;
using Isoplane.Abi;

namespace Isoplane;

/// <summary>
/// Names and values fixed for one deployed version of the application, which it can read only while
/// it starts.
/// </summary>
/// <remarks>
/// Most applications do not need this type. Settings and credentials come from
/// <see cref="RuntimeConfiguration"/>, or from <c>IConfiguration</c> in an ASP.NET application. These
/// values differ in two ways: they belong to the deployed version and cannot change without
/// deploying again, and they are saved in the template every instance starts from, so they must not
/// be secret.
/// </remarks>
public static class DeploymentConfiguration
{
    /// <summary>Reads the deployment's variables.</summary>
    /// <returns>
    /// The variables as name and value pairs, in the order the deployment lists them. A name can
    /// appear more than once. The list is empty when the deployment defines none.
    /// </returns>
    /// <remarks>
    /// Call it while the application starts, from an <see cref="InitializerAttribute"/> method or an
    /// <see cref="Initialization.Initializing"/> callback, and keep what you need in a static field.
    /// The values cannot be read once an instance is handling requests: a request that tries fails.
    /// </remarks>
    public static IReadOnlyList<KeyValuePair<string, string>> ReadVariables()
    {
        byte[] data = Abi.Initialization.ReadConfiguration();
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
