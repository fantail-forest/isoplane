using System.Collections.Generic;
using System.Collections.Immutable;
using System.Linq;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Diagnostics;

namespace Isoplane.Analyzers;

/// <summary>
/// Reports, on the method and as it is typed, each requirement of an entry attribute that the
/// method it marks does not meet.
/// </summary>
[DiagnosticAnalyzer(LanguageNames.CSharp)]
public sealed class EntryMethodAnalyzer : DiagnosticAnalyzer
{
    /// <inheritdoc />
    public override ImmutableArray<DiagnosticDescriptor> SupportedDiagnostics => EntryMethodRules.All;

    /// <inheritdoc />
    public override void Initialize(AnalysisContext context)
    {
        context.ConfigureGeneratedCodeAnalysis(GeneratedCodeAnalysisFlags.None);
        context.EnableConcurrentExecution();
        context.RegisterCompilationStartAction(start =>
        {
            if (EntryTypes.Find(start.Compilation) is { } types)
                start.RegisterSymbolAction(symbol => Analyze(symbol, types), SymbolKind.Method);
        });
    }

    private static void Analyze(SymbolAnalysisContext context, EntryTypes types)
    {
        var method = (IMethodSymbol)context.Symbol;
        foreach (var attribute in method.GetAttributes())
        {
            if (attribute.AttributeClass is not { } marker || !types.IsEntry(marker)) continue;
            string name = marker.Name.Substring(0, marker.Name.Length - "Attribute".Length);
            var declaration = method.DeclaringSyntaxReferences.Select(reference => reference.GetSyntax(context.CancellationToken))
                .OfType<MethodDeclarationSyntax>().FirstOrDefault();
            var onAttribute = attribute.ApplicationSyntaxReference?.GetSyntax(context.CancellationToken).GetLocation() ?? method.Locations[0];
            var onName = declaration?.Identifier.GetLocation() ?? method.Locations[0];

            if (context.Compilation.Options.OutputKind != OutputKind.DynamicallyLinkedLibrary)
            {
                // Nothing else about a method that is never called needs fixing.
                context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.NeverCalled, onAttribute, method.Name, name));
                continue;
            }
            if (!method.IsStatic)
                context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.MustBeStatic, onName, method.Name, name, method.ContainingType.Name));
            if (method.IsGenericMethod || ContainingTypes(method).Any(type => type.IsGenericType))
                context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.Generic, onName, method.Name, name));
            if (method.ContainingType.GetMembers(method.Name).OfType<IMethodSymbol>().Count() > 1)
                context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.Overloaded, onName, method.Name, name, method.ContainingType.Name));

            var onReturn = declaration?.ReturnType.GetLocation() ?? onName;
            var onParameters = declaration?.ParameterList.GetLocation() ?? onName;
            string returns = method.ReturnType.ToDisplayString(SymbolDisplayFormat.MinimallyQualifiedFormat);
            if (types.IsHandler(marker))
            {
                if (!types.IsResponseTask(method.ReturnType))
                    context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.HandlerReturn, onReturn, method.Name, returns));
                if (!types.AreHandlerParameters(method.Parameters))
                    context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.HandlerParameters, onParameters, method.Name));
            }
            else
            {
                if (!types.IsTask(method.ReturnType))
                    context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.LifecycleReturn, onReturn, method.Name, name, returns));
                if (method.Parameters.Length != 0)
                    context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.LifecycleParameters, onParameters, method.Name, name));
            }

            // Looked for only from a method that carries the attribute, so the search of the whole
            // assembly runs a handful of times however large the application is.
            var other = Marked(context.Compilation.Assembly.GlobalNamespace, marker)
                .FirstOrDefault(candidate => !SymbolEqualityComparer.Default.Equals(candidate, method));
            if (other is not null)
                context.ReportDiagnostic(Diagnostic.Create(EntryMethodRules.Duplicate, onAttribute, method.Name, name,
                    other.ContainingType.ToDisplayString(SymbolDisplayFormat.MinimallyQualifiedFormat) + "." + other.Name));
        }
    }

    private static IEnumerable<INamedTypeSymbol> ContainingTypes(IMethodSymbol method)
    {
        for (var type = method.ContainingType; type is not null; type = type.ContainingType) yield return type;
    }

    /// <summary>Every method of the assembly's own types that <paramref name="marker"/> marks.</summary>
    private static IEnumerable<IMethodSymbol> Marked(INamespaceOrTypeSymbol scope, INamedTypeSymbol marker)
    {
        foreach (var member in scope.GetMembers())
        {
            if (member is INamespaceOrTypeSymbol nested)
                foreach (var found in Marked(nested, marker)) yield return found;
            else if (member is IMethodSymbol method && method.GetAttributes().Any(attribute =>
                SymbolEqualityComparer.Default.Equals(attribute.AttributeClass, marker)))
                yield return method;
        }
    }
}
