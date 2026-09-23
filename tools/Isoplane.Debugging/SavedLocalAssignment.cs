using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;

namespace Isoplane.Debugging;

internal static class SavedLocalAssignment
{
    internal static void RequireLiteral(string expression)
    {
        var syntax = SyntaxFactory.ParseExpression(expression);
        bool literal = syntax is LiteralExpressionSyntax
            || syntax is PrefixUnaryExpressionSyntax { Operand: LiteralExpressionSyntax } unary
                && (unary.IsKind(SyntaxKind.UnaryMinusExpression) || unary.IsKind(SyntaxKind.UnaryPlusExpression));
        if (syntax.ContainsDiagnostics || !literal)
            throw new ProtocolException("Assign a literal value in a suspended async caller; expression evaluation requires a live frame.");
    }
}
