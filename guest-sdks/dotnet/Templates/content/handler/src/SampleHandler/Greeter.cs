using System.Text.Json;
using System.Text.Json.Serialization;

namespace SampleHandler;

public sealed record Greeting(string Message, string Name);

public sealed record ServiceInfo(string Application, Guid Instance);

/// <summary>Application logic with no dependency on Isoplane, so ordinary unit tests cover it.</summary>
public sealed class Greeter(string defaultName)
{
    public Greeting Greet(string? name)
    {
        name = string.IsNullOrWhiteSpace(name) ? defaultName : name.Trim();
        return new Greeting($"Hello, {name}!", name);
    }
}

/// <summary>Source-generated serialization, which needs no reflection at run time.</summary>
[JsonSerializable(typeof(Greeting))]
[JsonSerializable(typeof(ServiceInfo))]
[JsonSourceGenerationOptions(JsonSerializerDefaults.Web)]
public sealed partial class HandlerJson : JsonSerializerContext;
