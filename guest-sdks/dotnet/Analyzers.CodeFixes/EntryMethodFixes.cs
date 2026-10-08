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
/// Offers the change that satisfies an entry attribute where one change does: making the method
/// static, giving it the return type or parameters the attribute requires, or removing an
/// attribute that cannot apply. A diagnostic with more than one reasonable repair gets no fix.
/// </summary>
[ExportCodeFixProvider(LanguageNames.CSharp, Name = nameof(EntryMethodFixes)), Shared]
public sealed class EntryMethodFixes : CodeFixProvider
{
    /// <inheritdoc />
    public override ImmutableArray<string> FixableDiagnosticIds => ImmutableArray.Create(
        EntryMethodRules.MustBeStatic.Id, EntryMethodRules.HandlerReturn.Id, EntryMethodRules.HandlerParameters.Id,
        EntryMethodRules.LifecycleReturn.Id, EntryMethodRules.LifecycleParameters.Id,
        EntryMethodRules.Duplicate.Id, EntryMethodRules.NeverCalled.Id);

    /// <inheritdoc />
    public override FixAllProvider GetFixAllProvider() => WellKnownFixAllProviders.BatchFixer;

    /// <inheritdoc />
    public override async Task RegisterCodeFixesAsync(CodeFixContext context)
    {
        var root = await context.Document.GetSyntaxRootAsync(context.CancellationToken).ConfigureAwait(false);
        var model = await context.Document.GetSemanticModelAsync(context.CancellationToken).ConfigureAwait(false);
        if (root is null || model is null || EntryTypes.Find(model.Compilation) is not { } types) return;
        foreach (var diagnostic in context.Diagnostics)
        {
            var node = root.FindNode(diagnostic.Location.SourceSpan);
            if (node.FirstAncestorOrSelf<MethodDeclarationSyntax>() is not { } method
                || model.GetDeclaredSymbol(method, context.CancellationToken) is not { } symbol) continue;
            var fix = diagnostic.Id switch
            {
                "ISOPLANE1010" => Fix("Make the method static", (editor, _) =>
                    editor.SetModifiers(method, editor.Generator.GetModifiers(method).WithIsStatic(true))),
                "ISOPLANE1011" when ResponseTaskFix(method, symbol, types, model) is { } change => Fix("Return Task<Response>", change),
                "ISOPLANE1012" when method.ParameterList.Parameters.Count == 0 && types.Request is { } request => Fix("Add a Request parameter",
                    (editor, _) => editor.AddParameter(method, editor.Generator.ParameterDeclaration("request", editor.Generator.TypeExpression(request)))),
                "ISOPLANE1013" when TaskFix(method, symbol, types, model) is { } change => Fix("Return Task", change),
                "ISOPLANE1014" when !UsesParameters(method, symbol, model, context.CancellationToken) => Fix("Remove the parameters", (editor, _) =>
                    editor.ReplaceNode(method.ParameterList, method.ParameterList.WithParameters(default))),
                "ISOPLANE1017" or "ISOPLANE1018" when node.FirstAncestorOrSelf<AttributeSyntax>() is { } attribute => Fix(
                    $"Remove [{attribute.Name}]", (editor, _) => editor.ReplaceNode(method, Without(method, attribute))),
                _ => null
            };
            if (fix is not null) context.RegisterCodeFix(fix, diagnostic);

            CodeAction Fix(string title, System.Action<DocumentEditor, CancellationToken> change) => CodeAction.Create(title,
                cancellation => Edit.ApplyAsync(context.Document, editor => change(editor, cancellation), cancellation), equivalenceKey: diagnostic.Id);
        }
    }

    /// <summary>
    /// The change that makes a handler return <c>Task&lt;Response&gt;</c>: only the declared type when the
    /// method is asynchronous, and each returned <c>Response</c> wrapped in a completed task when it
    /// returns one directly. Null for any other return type, which has no single repair.
    /// </summary>
    private static System.Action<DocumentEditor, CancellationToken>? ResponseTaskFix(MethodDeclarationSyntax method, IMethodSymbol symbol, EntryTypes types, SemanticModel model)
    {
        if (types.ResponseTask is not { } responseTask || types.Task is not { } task) return null;
        if (symbol.IsAsync) return (editor, _) => editor.ReplaceNode(method.ReturnType, Type(editor, responseTask, method.ReturnType));
        if (!types.IsResponse(symbol.ReturnType)) return null;
        return (editor, _) =>
        {
            ExpressionSyntax Completed(ExpressionSyntax value) => (ExpressionSyntax)editor.Generator.InvocationExpression(
                editor.Generator.MemberAccessExpression(editor.Generator.TypeExpression(task), "FromResult"), value.WithoutTrivia());
            editor.ReplaceNode(method, new Returns(value => value is null ? null : Completed(value).WithTriviaFrom(value))
                .Rewrite(method).WithReturnType(Type(editor, responseTask, method.ReturnType)));
        };
    }

    /// <summary>
    /// The change that makes a lifecycle method return <c>Task</c>: only the declared type when the method
    /// is asynchronous, and a completed task returned wherever a <c>void</c> method ends. Null otherwise.
    /// </summary>
    private static System.Action<DocumentEditor, CancellationToken>? TaskFix(MethodDeclarationSyntax method, IMethodSymbol symbol, EntryTypes types, SemanticModel model)
    {
        if (types.Task is not { } task) return null;
        if (symbol.IsAsync) return (editor, _) => editor.ReplaceNode(method.ReturnType, Type(editor, task, method.ReturnType));
        if (!symbol.ReturnsVoid) return null;
        bool reachesEnd = method.Body is { } body && model.AnalyzeControlFlow(body) is { Succeeded: true, EndPointIsReachable: true };
        return (editor, _) =>
        {
            var completed = (ExpressionSyntax)editor.Generator.MemberAccessExpression(editor.Generator.TypeExpression(task), "CompletedTask");
            var returning = SyntaxFactory.ReturnStatement(completed).WithAdditionalAnnotations(Formatter.Annotation);
            var rewritten = new Returns(value => value ?? completed).Rewrite(method);
            if (rewritten.ExpressionBody is { } arrow)
                // An expression-bodied void method evaluates its expression for effect; it keeps doing so, then returns.
                rewritten = rewritten.WithExpressionBody(null).WithSemicolonToken(default)
                    .WithBody(SyntaxFactory.Block(SyntaxFactory.ExpressionStatement(arrow.Expression), returning).WithAdditionalAnnotations(Formatter.Annotation));
            else if (reachesEnd && rewritten.Body is { } block)
                rewritten = rewritten.WithBody(block.AddStatements(returning));
            editor.ReplaceNode(method, rewritten.WithReturnType(Type(editor, task, method.ReturnType)));
        };
    }

    /// <summary>The method without <paramref name="attribute"/>, keeping the blank lines and indentation that preceded it.</summary>
    private static MethodDeclarationSyntax Without(MethodDeclarationSyntax method, AttributeSyntax attribute)
    {
        var list = (AttributeListSyntax)attribute.Parent!;
        if (list.Attributes.Count > 1)
            return method.ReplaceNode(list, list.WithAttributes(list.Attributes.Remove(attribute)));
        var remaining = method.WithAttributeLists(method.AttributeLists.Remove(list));
        return method.AttributeLists.IndexOf(list) == 0 ? remaining.WithLeadingTrivia(list.GetLeadingTrivia()) : remaining;
    }

    private static TypeSyntax Type(DocumentEditor editor, ITypeSymbol type, TypeSyntax replaced) =>
        ((TypeSyntax)editor.Generator.TypeExpression(type)).WithTriviaFrom(replaced);

    private static bool UsesParameters(MethodDeclarationSyntax method, IMethodSymbol symbol, SemanticModel model, CancellationToken cancellation) =>
        ((SyntaxNode?)method.Body ?? method.ExpressionBody)?.DescendantNodes().OfType<IdentifierNameSyntax>().Any(name =>
            model.GetSymbolInfo(name, cancellation).Symbol is IParameterSymbol parameter
            && SymbolEqualityComparer.Default.Equals(parameter.ContainingSymbol, symbol)) == true;

    /// <summary>
    /// Rewrites what a method itself returns: each <c>return</c> statement and an expression body, but
    /// nothing inside a lambda or local function, whose returns are their own.
    /// </summary>
    private sealed class Returns(System.Func<ExpressionSyntax?, ExpressionSyntax?> replace) : CSharpSyntaxRewriter
    {
        internal MethodDeclarationSyntax Rewrite(MethodDeclarationSyntax method)
        {
            if (method.ExpressionBody is { } arrow && replace(arrow.Expression) is { } value)
                return method.WithExpressionBody(arrow.WithExpression(value));
            return method.Body is { } body ? method.WithBody((BlockSyntax)Visit(body)) : method;
        }

        public override SyntaxNode? VisitReturnStatement(ReturnStatementSyntax node) =>
            replace(node.Expression) is { } value ? node.WithExpression(value.WithLeadingTrivia(SyntaxFactory.Space)) : node;

        public override SyntaxNode? VisitParenthesizedLambdaExpression(ParenthesizedLambdaExpressionSyntax node) => node;
        public override SyntaxNode? VisitSimpleLambdaExpression(SimpleLambdaExpressionSyntax node) => node;
        public override SyntaxNode? VisitAnonymousMethodExpression(AnonymousMethodExpressionSyntax node) => node;
        public override SyntaxNode? VisitLocalFunctionStatement(LocalFunctionStatementSyntax node) => node;
    }
}
