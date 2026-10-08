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
using Microsoft.CodeAnalysis.Formatting;

namespace Isoplane.Analyzers;

/// <summary>
/// Replaces a call that blocks with the await that does the same work: <c>Run</c> and a blocked-on
/// <c>RunAsync</c> with <c>await RunAsync</c>, and a synchronous wait with its asynchronous
/// counterpart. Where the call is in a method that is not yet asynchronous, the fix makes it so
/// when that changes nothing but the method's own declaration.
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
            // Each of these calls returns nothing, so the await takes the place of a whole statement.
            if (root.FindNode(diagnostic.Location.SourceSpan, getInnermostNodeForTie: true) is not ExpressionSyntax blocking
                || blocking.Parent is not ExpressionStatementSyntax
                || Awaited(diagnostic.Id, blocking, model, SyntaxGenerator.GetGenerator(context.Document), context.CancellationToken) is not { } awaited
                || AwaitSite.Find(blocking, model) is not { } site) continue;
            string title = site.Method is null ? awaited.Title : $"{awaited.Title} and make '{site.Method.Identifier.Text}' asynchronous";
            context.RegisterCodeFix(CodeAction.Create(title, cancellation => Edit.ApplyAsync(context.Document, editor =>
            {
                var awaiting = ((ExpressionSyntax)editor.Generator.AwaitExpression(awaited.Task.WithoutTrivia())).WithTriviaFrom(blocking);
                if (site.Method is null) editor.ReplaceNode(blocking, awaiting);
                else editor.ReplaceNode(site.Method, site.MakeAsynchronous(site.Method.ReplaceNode(blocking, awaiting), editor.Generator));
            }, cancellation), equivalenceKey: diagnostic.Id), diagnostic);
        }
    }

    /// <summary>The task to await in place of <paramref name="blocking"/>, or null when the call has no asynchronous counterpart to offer.</summary>
    private static (ExpressionSyntax Task, string Title)? Awaited(string id, ExpressionSyntax blocking, SemanticModel model, SyntaxGenerator generator, CancellationToken cancellation)
    {
        if (id == HostRunAnalyzer.NotAwaited.Id) return (HostTask(blocking), "Await RunAsync");
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
            ("System.Threading.SemaphoreSlim", "Wait") when method.Parameters.Length == 0
                || method.Parameters.Length == 1 && method.Parameters[0].Type.Name == "CancellationToken" => (Renamed("WaitAsync"), "Await WaitAsync"),
            _ => null
        };
    }

    /// <summary>The <c>RunAsync</c> call inside a statement that discards its task or blocks on it.</summary>
    private static ExpressionSyntax HostTask(ExpressionSyntax blocking) => blocking switch
    {
        InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax { Name.Identifier.Text: "Wait" } wait } => wait.Expression,
        InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax { Name.Identifier.Text: "GetResult",
            Expression: InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax { Name.Identifier.Text: "GetAwaiter" } awaiter } } } => awaiter.Expression,
        _ => blocking
    };

    /// <summary>
    /// Where an await can be written: in code that is already asynchronous, or in a method this fix can
    /// make asynchronous by changing only its declaration.
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

        internal static AwaitSite? Find(SyntaxNode node, SemanticModel model)
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
                        // A method that returns nothing can return a task instead without its body changing.
                        // Any other return type would change every return statement and every caller.
                        var task = model.Compilation.GetTypeByMetadataName("System.Threading.Tasks.Task");
                        return task is not null && model.GetDeclaredSymbol(method) is { ReturnsVoid: true } ? new(method, task) : null;
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
