using System.Text.Json;
using System.Text.Json.Serialization;

namespace Isoplane.Build;

internal enum GuestExecution { Interpreted }

/// <summary>A package's <c>application.json</c>: the entry assembly and what the runtime invokes in it.</summary>
internal sealed record ManagedApplication(string Assembly, ApplicationEntry Entry, GuestExecution Execution);

/// <summary>A static method, by its declaring type's reflection name.</summary>
internal sealed record MethodReference(string Type, string Method)
{
    public override string ToString() => Type.Replace('+', '.') + "." + Method;
}

[JsonPolymorphic(TypeDiscriminatorPropertyName = "kind")]
[JsonDerivedType(typeof(EntryPointApplication), "entryPoint")]
[JsonDerivedType(typeof(HandlerApplication), "handler")]
[JsonDerivedType(typeof(FactoryApplication), "factory")]
internal abstract record ApplicationEntry;

/// <summary>An executable whose asynchronous <c>Main</c> runs until it awaits its host.</summary>
internal sealed record EntryPointApplication(MethodReference Main) : ApplicationEntry;

/// <summary>A library whose attributed static methods receive dispatches and lifecycle events.</summary>
internal sealed record HandlerApplication(MethodReference Http, MethodReference? Initializer, MethodReference? Activated) : ApplicationEntry;

/// <summary>A library that names a method returning its router, handler or ASP.NET application.</summary>
internal sealed record FactoryApplication(string Factory) : ApplicationEntry;

internal static class BuildJson
{
    internal static readonly JsonSerializerOptions Options = new()
    {
        PropertyNameCaseInsensitive = true,
        RespectRequiredConstructorParameters = true,
        RespectNullableAnnotations = true,
        UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow,
        Converters = { new JsonStringEnumConverter<GuestExecution>(allowIntegerValues: false) }
    };
}
