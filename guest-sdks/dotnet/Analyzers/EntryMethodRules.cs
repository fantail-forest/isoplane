using System.Collections.Immutable;
using Microsoft.CodeAnalysis;

namespace Isoplane.Analyzers;

/// <summary>
/// What <c>[HttpHandler]</c>, <c>[Initializer]</c> and <c>[Activated]</c> require of the method they
/// mark, one diagnostic for each way a method can fall short. The build checks the same
/// requirements again on the compiled assembly, as <c>ISOPLANE1005</c>.
/// </summary>
internal static class EntryMethodRules
{
    private const string Category = "Isoplane";

    internal static readonly DiagnosticDescriptor MustBeStatic = Rule("ISOPLANE1010", "An entry method must be static",
        "'{0}' is marked [{1}], so it must be static: Isoplane calls it without creating an instance of '{2}'");

    internal static readonly DiagnosticDescriptor HandlerReturn = Rule("ISOPLANE1011", "An [HttpHandler] method must return Task<Response>",
        "The [HttpHandler] method '{0}' returns '{1}'; it must return Task<Response>");

    internal static readonly DiagnosticDescriptor HandlerParameters = Rule("ISOPLANE1012", "An [HttpHandler] method takes a request",
        "The [HttpHandler] method '{0}' must take a Request, or a StreamingRequest to read the body as it arrives, optionally followed by a Ctx");

    internal static readonly DiagnosticDescriptor LifecycleReturn = Rule("ISOPLANE1013", "A lifecycle method must return Task",
        "The [{1}] method '{0}' returns '{2}'; it must return Task");

    internal static readonly DiagnosticDescriptor LifecycleParameters = Rule("ISOPLANE1014", "A lifecycle method takes no parameters",
        "The [{1}] method '{0}' must take no parameters: Isoplane has nothing to pass it");

    internal static readonly DiagnosticDescriptor Generic = Rule("ISOPLANE1015", "An entry method cannot be generic",
        "The [{1}] method '{0}' must be a non-generic method of a non-generic type: Isoplane has no type arguments to call it with");

    internal static readonly DiagnosticDescriptor Overloaded = Rule("ISOPLANE1016", "An entry method cannot be overloaded",
        "The [{1}] method '{0}' must not be overloaded: Isoplane finds it by name. Rename the other '{0}' methods of '{2}'.");

    internal static readonly DiagnosticDescriptor Duplicate = Rule("ISOPLANE1017", "An application has one method for each entry attribute",
        "An application has one [{1}] method, and '{2}' is also marked. Keep the attribute on one and call the other from it.");

    internal static readonly DiagnosticDescriptor NeverCalled = Rule("ISOPLANE1018", "An entry attribute has no effect in an application with an entry point",
        "This project has an entry point, so the [{1}] method '{0}' would never be called. " +
        "Remove the attribute, or subscribe to Isoplane.Initialization.Initializing and Isoplane.Activation.Activated before awaiting RunAsync.");

    internal static readonly ImmutableArray<DiagnosticDescriptor> All = ImmutableArray.Create(
        MustBeStatic, HandlerReturn, HandlerParameters, LifecycleReturn, LifecycleParameters, Generic, Overloaded, Duplicate, NeverCalled);

    private static DiagnosticDescriptor Rule(string id, string title, string message) =>
        new(id, title, message, Category, DiagnosticSeverity.Error, isEnabledByDefault: true);
}
