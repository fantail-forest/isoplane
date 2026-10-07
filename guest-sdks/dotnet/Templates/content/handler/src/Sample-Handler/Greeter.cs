using System.Text.Json;
using System.Text.Json.Serialization;

namespace Sample_Handler;

public sealed record Greeting(string Message, string Name);

public sealed record ServiceInfo(string Application, Guid Instance);

/// <summary>
/// Application logic with no dependency on Isoplane, so ordinary unit tests cover
/// it. Keeping logic in classes like this one, and the handler thin, is what
/// lets most tests run without the sandbox.
/// </summary>
public sealed class Greeter(string defaultName)
{
    public Greeting Greet(string? name)
    {
        name = string.IsNullOrWhiteSpace(name) ? defaultName : name.Trim();
        return new Greeting($"Hello, {name}!", name);
    }
}

/// <summary>
/// Source-generated serialization: the serializer for each listed type is built
/// at compile time. Add a <c>[JsonSerializable]</c> line for each type the
/// application sends or receives.
/// </summary>
[JsonSerializable(typeof(Greeting))]
[JsonSerializable(typeof(ServiceInfo))]
[JsonSourceGenerationOptions(JsonSerializerDefaults.Web)]
public sealed partial class HandlerJson : JsonSerializerContext;
