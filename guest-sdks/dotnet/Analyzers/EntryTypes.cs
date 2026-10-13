using System.Collections.Immutable;
using Microsoft.CodeAnalysis;

namespace Isoplane.Analyzers;

/// <summary>The SDK and framework types the entry attributes' requirements are stated in.</summary>
internal sealed class EntryTypes
{
    private readonly INamedTypeSymbol handler, initializer, activated;
    private readonly INamedTypeSymbol? request, streamingRequest, response, task, taskOf;

    private EntryTypes(Compilation compilation, INamedTypeSymbol handler, INamedTypeSymbol initializer, INamedTypeSymbol activated)
    {
        this.handler = handler;
        this.initializer = initializer;
        this.activated = activated;
        request = compilation.GetTypeByMetadataName("Isoplane.Request");
        streamingRequest = compilation.GetTypeByMetadataName("Isoplane.StreamingRequest");
        response = compilation.GetTypeByMetadataName("Isoplane.Response");
        task = compilation.GetTypeByMetadataName("System.Threading.Tasks.Task");
        taskOf = compilation.GetTypeByMetadataName("System.Threading.Tasks.Task`1");
    }

    /// <summary>The types as <paramref name="compilation"/> sees them, or null when it does not reference the Isoplane SDK.</summary>
    internal static EntryTypes? Find(Compilation compilation) =>
        compilation.GetTypeByMetadataName("Isoplane.HttpHandlerAttribute") is { } handler
        && compilation.GetTypeByMetadataName("Isoplane.InitializerAttribute") is { } initializer
        && compilation.GetTypeByMetadataName("Isoplane.ActivatedAttribute") is { } activated
            ? new(compilation, handler, initializer, activated) : null;

    internal bool IsEntry(INamedTypeSymbol attribute) => IsHandler(attribute) || Same(attribute, initializer) || Same(attribute, activated);

    internal bool IsHandler(INamedTypeSymbol attribute) => Same(attribute, handler);

    internal bool IsTask(ITypeSymbol type) => Same(type, task);

    internal bool IsResponse(ITypeSymbol type) => Same(type, response);

    internal bool IsResponseTask(ITypeSymbol type) =>
        type is INamedTypeSymbol { TypeArguments.Length: 1 } named && Same(named.OriginalDefinition, taskOf) && Same(named.TypeArguments[0], response);

    /// <summary>One <c>Request</c> or <c>StreamingRequest</c>, passed by value.</summary>
    internal bool AreHandlerParameters(ImmutableArray<IParameterSymbol> parameters) =>
        parameters.Length == 1 && parameters[0].RefKind == RefKind.None
        && (Same(parameters[0].Type, request) || Same(parameters[0].Type, streamingRequest));

    /// <summary><c>Task&lt;Response&gt;</c>, or null when the compilation lacks either type.</summary>
    internal INamedTypeSymbol? ResponseTask => taskOf is null || response is null ? null : taskOf.Construct(response);

    internal INamedTypeSymbol? Task => task;

    internal INamedTypeSymbol? Request => request;

    private static bool Same(ISymbol? left, ISymbol? right) => left is not null && SymbolEqualityComparer.Default.Equals(left, right);
}
