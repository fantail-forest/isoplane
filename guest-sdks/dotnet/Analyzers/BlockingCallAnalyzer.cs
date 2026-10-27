using System.Collections.Generic;
using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Diagnostics;
using Microsoft.CodeAnalysis.Operations;

namespace Isoplane.Analyzers;

/// <summary>
/// Reports, where they are written, starting a thread (<c>ISOPLANE1002</c>) and waiting synchronously
/// (<c>ISOPLANE1003</c>). The build reports the same members from the compiled assembly, except an
/// awaiter's <c>GetResult</c>, which every <c>await</c> compiles to and only source tells apart.
/// </summary>
[DiagnosticAnalyzer(LanguageNames.CSharp)]
public sealed class BlockingCallAnalyzer : DiagnosticAnalyzer
{
    internal static readonly DiagnosticDescriptor StartsThread = new("ISOPLANE1002", "A guest application cannot start a thread",
        "'{0}' starts an operating-system thread, which a guest application cannot do. Use asynchronous .NET APIs for concurrent I/O.",
        "Isoplane", DiagnosticSeverity.Warning, isEnabledByDefault: true);

    internal static readonly DiagnosticDescriptor Waits = new("ISOPLANE1003", "A guest application cannot wait synchronously",
        "'{0}' can wait synchronously, and a guest cannot block waiting for unfinished work. Use await and asynchronous wait APIs.",
        "Isoplane", DiagnosticSeverity.Warning, isEnabledByDefault: true);

    /// <inheritdoc />
    public override ImmutableArray<DiagnosticDescriptor> SupportedDiagnostics => ImmutableArray.Create(StartsThread, Waits);

    /// <inheritdoc />
    public override void Initialize(AnalysisContext context)
    {
        // The build's check reads every method of the assembly, generated or written, so this does too.
        context.ConfigureGeneratedCodeAnalysis(GeneratedCodeAnalysisFlags.Analyze | GeneratedCodeAnalysisFlags.ReportDiagnostics);
        context.EnableConcurrentExecution();
        context.RegisterCompilationStartAction(start =>
        {
            var hosts = HostTypes.Find(start.Compilation);
            start.RegisterOperationAction(operation =>
            {
                ISymbol? member = operation.Operation switch
                {
                    IInvocationOperation invocation => invocation.TargetMethod,
                    IMethodReferenceOperation reference => reference.Method,
                    IPropertyReferenceOperation property => property.Property,
                    _ => null
                };
                if (member is null || Rule(member) is not { } rule) return;
                // Blocking on the host's RunAsync has its own diagnostic, which says what to do about it.
                if (operation.Operation is IInvocationOperation { Instance: { } awaited } && hosts is not null && IsHostRun(awaited, hosts)) return;
                operation.ReportDiagnostic(Diagnostic.Create(rule, operation.Operation.Syntax.GetLocation(),
                    member.ContainingType.Name + "." + member.Name));
            }, OperationKind.Invocation, OperationKind.MethodReference, OperationKind.PropertyReference);
        });
    }

    private static bool IsHostRun(IOperation task, HostTypes hosts)
    {
        while (task is IInvocationOperation { TargetMethod.Name: "ConfigureAwait" or "GetAwaiter", Instance: { } inner }) task = inner;
        return task is IInvocationOperation run && hosts.IsRunAsync(run.TargetMethod);
    }

    /// <summary>
    /// The rule a use of <paramref name="member"/>, a method or property, breaks, by the declaring type's
    /// metadata name and the member's name.
    /// </summary>
    internal static DiagnosticDescriptor? Rule(ISymbol member) =>
        (MetadataName(((member as IMethodSymbol)?.ReducedFrom ?? member).ContainingType), member.Name) switch
        {
            ("System.Threading.Thread", "Start") => StartsThread,
            ("System.Threading.Thread", "Sleep" or "Join") => Waits,
            ("System.Threading.Monitor", "Wait") => Waits,
            ("System.Threading.Tasks.Task", "Wait" or "WaitAll" or "WaitAny") => Waits,
            ("System.Threading.Tasks.Task`1" or "System.Threading.Tasks.ValueTask`1", "Result") => Waits,
            ("System.Threading.SemaphoreSlim", "Wait") => Waits,
            ("System.Runtime.CompilerServices.TaskAwaiter" or "System.Runtime.CompilerServices.TaskAwaiter`1"
                or "System.Runtime.CompilerServices.ValueTaskAwaiter" or "System.Runtime.CompilerServices.ValueTaskAwaiter`1"
                or "System.Runtime.CompilerServices.ConfiguredTaskAwaitable.ConfiguredTaskAwaiter"
                or "System.Runtime.CompilerServices.ConfiguredTaskAwaitable`1.ConfiguredTaskAwaiter"
                or "System.Runtime.CompilerServices.ConfiguredValueTaskAwaitable.ConfiguredValueTaskAwaiter"
                or "System.Runtime.CompilerServices.ConfiguredValueTaskAwaitable`1.ConfiguredValueTaskAwaiter", "GetResult") => Waits,
            _ => null
        };

    /// <summary>
    /// The name of <paramref name="type"/> as assembly metadata spells it, which the build's inspection of
    /// the compiled assembly matches on: namespace, then each enclosing type and the type, with the arity
    /// suffix of a generic type and none of its arguments.
    /// </summary>
    private static string? MetadataName(INamedTypeSymbol? type)
    {
        if (type is null) return null;
        var names = new List<string>();
        for (var current = type.OriginalDefinition; current is not null; current = current.ContainingType)
            names.Insert(0, current.MetadataName);
        if (type.ContainingNamespace is { IsGlobalNamespace: false } ns) names.Insert(0, ns.ToDisplayString());
        return string.Join(".", names);
    }
}
