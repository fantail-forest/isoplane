using System.Collections.Immutable;
using Isoplane.Build;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Diagnostics;
using Microsoft.CodeAnalysis.Operations;

namespace Isoplane.Analyzers;

/// <summary>
/// Reports an ASP.NET application that blocks in <c>Run</c>, or that starts <c>RunAsync</c> and does
/// not await it. An Isoplane application returns to the platform while it waits for requests, so its
/// entry point awaits the host; the build refuses a synchronous entry point as <c>ISOPLANE1005</c>.
/// </summary>
[DiagnosticAnalyzer(LanguageNames.CSharp)]
public sealed class HostRunAnalyzer : DiagnosticAnalyzer
{
    internal static readonly DiagnosticDescriptor Blocks = new("ISOPLANE1020", "Run blocks until the application stops",
        "'{0}' blocks until the application stops, which an Isoplane application cannot do: it returns to the platform while it waits for requests. Await RunAsync instead.",
        "Isoplane", DiagnosticSeverity.Error, isEnabledByDefault: true, helpLinkUri: ErrorCodeReference.Link("ISOPLANE1020"));

    internal static readonly DiagnosticDescriptor NotAwaited = new("ISOPLANE1021", "RunAsync must be awaited",
        "The task RunAsync returns must be awaited: an entry point that returns or blocks while the application runs stops it from serving requests",
        "Isoplane", DiagnosticSeverity.Error, isEnabledByDefault: true, helpLinkUri: ErrorCodeReference.Link("ISOPLANE1021"));

    /// <inheritdoc />
    public override ImmutableArray<DiagnosticDescriptor> SupportedDiagnostics => ImmutableArray.Create(Blocks, NotAwaited);

    /// <inheritdoc />
    public override void Initialize(AnalysisContext context)
    {
        context.ConfigureGeneratedCodeAnalysis(GeneratedCodeAnalysisFlags.None);
        context.EnableConcurrentExecution();
        context.RegisterCompilationStartAction(start =>
        {
            var hosts = HostTypes.Find(start.Compilation);
            if (hosts is null) return;
            start.RegisterOperationAction(operation =>
            {
                var invocation = (IInvocationOperation)operation.Operation;
                if (hosts.IsRun(invocation.TargetMethod))
                    operation.ReportDiagnostic(Diagnostic.Create(Blocks, invocation.Syntax.GetLocation(), invocation.TargetMethod.Name));
                else if (hosts.IsRunAsync(invocation.TargetMethod) && HostTypes.Unobserved(invocation) is { } use)
                    operation.ReportDiagnostic(Diagnostic.Create(NotAwaited, use.Syntax.GetLocation()));
            }, OperationKind.Invocation);
        });
    }
}

/// <summary>The hosts whose <c>Run</c> and <c>RunAsync</c> start an application.</summary>
internal sealed class HostTypes
{
    private readonly INamedTypeSymbol? application, extensions;

    private HostTypes(INamedTypeSymbol? application, INamedTypeSymbol? extensions)
    {
        this.application = application;
        this.extensions = extensions;
    }

    /// <summary>The host types <paramref name="compilation"/> references, or null when it references none.</summary>
    internal static HostTypes? Find(Compilation compilation)
    {
        var application = compilation.GetTypeByMetadataName("Microsoft.AspNetCore.Builder.WebApplication");
        var extensions = compilation.GetTypeByMetadataName("Microsoft.Extensions.Hosting.HostingAbstractionsHostExtensions");
        return application is null && extensions is null ? null : new(application, extensions);
    }

    internal bool IsRun(IMethodSymbol method) => method.Name == "Run" && Declares(method);

    internal bool IsRunAsync(IMethodSymbol method) => method.Name == "RunAsync" && Declares(method);

    private bool Declares(IMethodSymbol method)
    {
        var type = (method.ReducedFrom ?? method).ContainingType;
        return SymbolEqualityComparer.Default.Equals(type, application) || SymbolEqualityComparer.Default.Equals(type, extensions);
    }

    /// <summary>
    /// The operation that drops or blocks on the task <paramref name="run"/> returns: the statement that
    /// discards it, or the <c>Wait()</c> or <c>GetAwaiter().GetResult()</c> around it. Null when the task
    /// is awaited, returned, stored or passed on, where it can still be awaited.
    /// </summary>
    internal static IOperation? Unobserved(IInvocationOperation run)
    {
        IOperation task = run;
        // ConfigureAwait keeps the task awaitable; what matters is what happens to its result.
        while (task.Parent is IInvocationOperation { TargetMethod.Name: "ConfigureAwait" } configured && configured.Instance == task) task = configured;
        return task.Parent switch
        {
            IExpressionStatementOperation => task,
            IInvocationOperation { TargetMethod.Name: "Wait" } wait when wait.Instance == task => wait,
            IInvocationOperation { TargetMethod.Name: "GetAwaiter" } awaiter when awaiter.Instance == task
                && awaiter.Parent is IInvocationOperation { TargetMethod.Name: "GetResult" } result && result.Instance == awaiter => result,
            _ => null
        };
    }
}
