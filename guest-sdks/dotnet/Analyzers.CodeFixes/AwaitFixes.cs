using System.Collections.Immutable;
using System.Composition;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CodeActions;
using Microsoft.CodeAnalysis.CodeFixes;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Editing;
using Microsoft.CodeAnalysis.FindSymbols;
using Microsoft.CodeAnalysis.Formatting;

namespace Isoplane.Analyzers;

/// <summary>
/// Replaces a call that blocks with the await that does the same work: <c>Run</c> and a blocked-on
/// <c>RunAsync</c> with <c>await RunAsync</c>, a synchronous wait with its asynchronous
/// counterpart, and a task's <c>Result</c> or its awaiter's <c>GetResult()</c> with awaiting the task.
/// An unreferenced synchronous program entry point can become asynchronous; other synchronous
/// methods keep their signatures and callers.
/// </summary>
[ExportCodeFixProvider(LanguageNames.CSharp, Name = nameof(AwaitFixes)), Shared]
public sealed class AwaitFixes : CodeFixProvider
{
    /// <inheritdoc />
    public override ImmutableArray<string> FixableDiagnosticIds =>
        ImmutableArray.Create(BlockingCallAnalyzer.Waits.Id, HostRunAnalyzer.Blocks.Id, HostRunAnalyzer.NotAwaited.Id);

    /// <inheritdoc />
    public override FixAllProvider GetFixAllProvider() => WellKnownFixAllProviders.BatchFixer;

    /// <inheritdoc />
    public override async Task RegisterCodeFixesAsync(CodeFixContext context)
    {
        var root = await context.Document.GetSyntaxRootAsync(context.CancellationToken).ConfigureAwait(false);
        var model = await context.Document.GetSemanticModelAsync(context.CancellationToken).ConfigureAwait(false);
        if (root is null || model is null) return;
        foreach (var diagnostic in context.Diagnostics)
        {
            if (root.FindNode(diagnostic.Location.SourceSpan, getInnermostNodeForTie: true) is not ExpressionSyntax blocking
                || Awaited(diagnostic.Id, blocking, model, SyntaxGenerator.GetGenerator(context.Document), context.CancellationToken) is not { } awaited
                || await AwaitSite.FindAsync(blocking, model, context.Document.Project.Solution, context.CancellationToken).ConfigureAwait(false) is not { } site) continue;
            string title = site.Method is null ? awaited.Title : $"{awaited.Title} and make '{site.Method.Identifier.Text}' asynchronous";
            var changed = await Edit.ApplyAsync(context.Document, editor =>
            {
                var awaiting = Awaiting(awaited.Task, blocking, editor.Generator);
                if (site.Method is null) editor.ReplaceNode(blocking, awaiting);
                else editor.ReplaceNode(site.Method, site.MakeAsynchronous(site.Method.ReplaceNode(blocking, awaiting), editor.Generator));
            }, context.CancellationToken).ConfigureAwait(false);
            var compilation = await changed.Project.GetCompilationAsync(context.CancellationToken).ConfigureAwait(false);
            if (compilation is null || compilation.GetDiagnostics(context.CancellationToken).Any(candidate => candidate.Severity == DiagnosticSeverity.Error)) continue;
            context.RegisterCodeFix(CodeAction.Create(title, _ => Task.FromResult(changed), equivalenceKey: diagnostic.Id), diagnostic);
        }
    }

    /// <summary>
    /// An await of <paramref name="task"/> in the place of <paramref name="blocking"/>, in parentheses
    /// where a member access, call, index or postfix operator on the result would otherwise bind to the task.
    /// </summary>
    private static ExpressionSyntax Awaiting(ExpressionSyntax task, ExpressionSyntax blocking, SyntaxGenerator generator)
    {
        var awaiting = (ExpressionSyntax)generator.AwaitExpression(task.WithoutTrivia());
        if (blocking.Parent is MemberAccessExpressionSyntax or ConditionalAccessExpressionSyntax or InvocationExpressionSyntax
            or ElementAccessExpressionSyntax or PostfixUnaryExpressionSyntax)
            awaiting = SyntaxFactory.ParenthesizedExpression(awaiting);
        return awaiting.WithTriviaFrom(blocking);
    }

    /// <summary>The task to await in place of <paramref name="blocking"/>, or null when the call has no asynchronous counterpart to offer.</summary>
    private static (ExpressionSyntax Task, string Title)? Awaited(string id, ExpressionSyntax blocking, SemanticModel model, SyntaxGenerator generator, CancellationToken cancellation)
    {
        if (id == HostRunAnalyzer.NotAwaited.Id) return (HostTask(blocking), "Await RunAsync");
        // ISOPLANE1003 reports Result only on a task, and GetResult only on the awaiter of one.
        if (blocking is MemberAccessExpressionSyntax read && model.GetSymbolInfo(read, cancellation).Symbol is IPropertySymbol { Name: "Result" })
            return (read.Expression, "Await the task");
        if (blocking is not InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax member } call
            || model.GetSymbolInfo(call, cancellation).Symbol is not IMethodSymbol method) return null;
        ExpressionSyntax Renamed(string name) => call.WithExpression(member.WithName(SyntaxFactory.IdentifierName(name)));
        if (id == HostRunAnalyzer.Blocks.Id) return (Renamed("RunAsync"), "Await RunAsync");
        return (method.ContainingType.ToDisplayString(), method.Name) switch
        {
            ("System.Threading.Thread", "Sleep") when method.Parameters.Length == 1
                && model.Compilation.GetTypeByMetadataName("System.Threading.Tasks.Task") is { } task =>
                (call.WithExpression((ExpressionSyntax)generator.MemberAccessExpression(generator.TypeExpression(task), "Delay")), "Await Task.Delay"),
            ("System.Threading.Tasks.Task", "Wait") when method.Parameters.Length == 0 => (member.Expression, "Await the task"),
            ("System.Threading.Tasks.Task", "WaitAll") when method.Parameters.Length == 1 && method.Parameters[0].IsParams => (Renamed("WhenAll"), "Await Task.WhenAll"),
            ("System.Threading.Tasks.Task", "WaitAny") when method.Parameters.Length == 1 && method.Parameters[0].IsParams => (Renamed("WhenAny"), "Await Task.WhenAny"),
            ("System.Threading.SemaphoreSlim", "Wait") when method.Parameters.Length == 0
                || method.Parameters.Length == 1 && method.Parameters[0].Type.Name == "CancellationToken" => (Renamed("WaitAsync"), "Await WaitAsync"),
            (_, "GetResult") when Awaitable(call) is { } awaitable => (awaitable, "Await the task"),
            _ => null
        };
    }

    /// <summary>The <c>RunAsync</c> call inside a statement that discards its task or blocks on it.</summary>
    private static ExpressionSyntax HostTask(ExpressionSyntax blocking) => blocking switch
    {
        InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax { Name.Identifier.Text: "Wait" } wait } => wait.Expression,
        InvocationExpressionSyntax call when Awaitable(call) is { } awaitable => awaitable,
        _ => blocking
    };

    /// <summary>The expression <paramref name="call"/> blocks on as <c>.GetAwaiter().GetResult()</c>, or null when it is not that call.</summary>
    private static ExpressionSyntax? Awaitable(InvocationExpressionSyntax call) => call.Expression is MemberAccessExpressionSyntax
    {
        Name.Identifier.Text: "GetResult",
        Expression: InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax { Name.Identifier.Text: "GetAwaiter" } awaiter }
    } ? awaiter.Expression : null;

    /// <summary>
    /// Where an await can be written without changing callers: in asynchronous code or an
    /// unreferenced program entry point whose task the runtime will await.
    /// </summary>
    private sealed class AwaitSite
    {
        private readonly ITypeSymbol? returns;

        private AwaitSite(MethodDeclarationSyntax? method, ITypeSymbol? returns)
        {
            Method = method;
            this.returns = returns;
        }

        /// <summary>The method to make asynchronous, or null when the code around the await already is.</summary>
        internal MethodDeclarationSyntax? Method { get; }

        internal static async Task<AwaitSite?> FindAsync(SyntaxNode node, SemanticModel model, Solution solution, CancellationToken cancellation)
        {
            foreach (var ancestor in node.Ancestors())
            {
                switch (ancestor)
                {
                    case AnonymousFunctionExpressionSyntax lambda:
                        return lambda.AsyncKeyword.IsKind(SyntaxKind.AsyncKeyword) ? new(null, null) : null;
                    case LocalFunctionStatementSyntax local:
                        return local.Modifiers.Any(SyntaxKind.AsyncKeyword) ? new(null, null) : null;
                    case GlobalStatementSyntax:
                        return new(null, null);
                    case MethodDeclarationSyntax method when method.Modifiers.Any(SyntaxKind.AsyncKeyword):
                        return new(null, null);
                    case MethodDeclarationSyntax method:
                        var symbol = model.GetDeclaredSymbol(method, cancellation);
                        if (symbol is not { ReturnsVoid: true }
                            || !SymbolEqualityComparer.Default.Equals(symbol, model.Compilation.GetEntryPoint(cancellation))
                            || model.Compilation.GetTypeByMetadataName("System.Threading.Tasks.Task") is not { } task)
                            return null;
                        // The runtime awaits an async Main, but ordinary calls and delegates would still need migration.
                        var references = await SymbolFinder.FindReferencesAsync(symbol, solution, cancellation).ConfigureAwait(false);
                        return references.Any(reference => reference.Locations.Any()) ? null : new(method, task);
                    case MemberDeclarationSyntax:
                        return null;
                }
            }
            return null;
        }

        internal MethodDeclarationSyntax MakeAsynchronous(MethodDeclarationSyntax method, SyntaxGenerator generator) =>
            ((MethodDeclarationSyntax)generator.WithModifiers(method, generator.GetModifiers(method).WithAsync(true)))
                .WithReturnType(((TypeSyntax)generator.TypeExpression(returns!)).WithTriviaFrom(method.ReturnType))
                .WithAdditionalAnnotations(Formatter.Annotation);
    }
}
