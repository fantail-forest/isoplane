using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Diagnostics;
using Microsoft.CodeAnalysis.Operations;

namespace Isoplane.Analyzers;

/// <summary>
/// Reports, where they are written, the calls the build reports from the compiled assembly as
/// <c>ISOPLANE1002</c> and <c>ISOPLANE1003</c>: starting a thread, and waiting synchronously.
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
                var method = operation.Operation switch
                {
                    IInvocationOperation invocation => invocation.TargetMethod,
                    IMethodReferenceOperation reference => reference.Method,
                    _ => null
                };
                if (method is null || Rule(method) is not { } rule) return;
                // Blocking on the host's RunAsync has its own diagnostic, which says what to do about it.
                if (operation.Operation is IInvocationOperation { Instance: { } awaited } && hosts is not null && IsHostRun(awaited, hosts)) return;
                operation.ReportDiagnostic(Diagnostic.Create(rule, operation.Operation.Syntax.GetLocation(),
                    method.ContainingType.Name + "." + method.Name));
            }, OperationKind.Invocation, OperationKind.MethodReference);
        });
    }

    private static bool IsHostRun(IOperation task, HostTypes hosts)
    {
        while (task is IInvocationOperation { TargetMethod.Name: "ConfigureAwait", Instance: { } inner }) task = inner;
        return task is IInvocationOperation run && hosts.IsRunAsync(run.TargetMethod);
    }

    /// <summary>The rule a call to <paramref name="method"/> breaks, by the method's declaring type and name.</summary>
    internal static DiagnosticDescriptor? Rule(IMethodSymbol method) =>
        ((method.ReducedFrom ?? method).ContainingType?.ToDisplayString(), method.Name) switch
        {
            ("System.Threading.Thread", "Start") => StartsThread,
            ("System.Threading.Thread", "Sleep" or "Join") => Waits,
            ("System.Threading.Monitor", "Wait") => Waits,
            ("System.Threading.Tasks.Task", "Wait" or "WaitAll" or "WaitAny") => Waits,
            ("System.Threading.SemaphoreSlim", "Wait") => Waits,
            _ => null
        };
}
