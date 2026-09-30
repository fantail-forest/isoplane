using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Mono.Debugger.Soft;
using Mono.Debugging.Client;
using Mono.Debugging.Soft;

namespace Isoplane.Debugging;

internal static class ManagedLocations
{
    internal static SoftEvaluationContext Context(SoftDebuggerSession session, int frameIndex)
    {
        var threads = session.VirtualMachine.GetThreads();
        if (threads.Count != 1) throw new ProtocolException("The local WASM debugger requires one guest thread.");
        var frames = threads[0].GetFrames();
        if (frameIndex < 0 || frameIndex >= frames.Length) throw new ProtocolException("The stack frame is not part of the current pause.");
        return new SoftEvaluationContext(session, frames[frameIndex], EvaluationOptions.DefaultOptions);
    }

    internal static ManagedDataBreakpoint Resolve(SoftEvaluationContext context, string expression, int id)
    {
        var syntax = SyntaxFactory.ParseExpression(expression);
        if (syntax.ContainsDiagnostics) throw new ProtocolException("A local, managed field or array element expression is required.");
        if (syntax is ElementAccessExpressionSyntax element) return Element(context, element, id);
        var reference = context.Evaluator.Evaluate(context, expression);
        try
        {
            return reference switch
            {
                FieldValueReference field => field.GetManagedDataBreakpoint(id),
                VariableValueReference local => local.GetManagedDataBreakpoint(id),
                _ => throw new ProtocolException("Data breakpoints require a local, managed field or array element.")
            };
        }
        catch (NotSupportedException error) { throw new ProtocolException(error.Message, error); }
    }

    private static object Evaluate(SoftEvaluationContext context, string expression) => context.Evaluator.Evaluate(context, expression).Value;

    private static ManagedDataBreakpoint Element(SoftEvaluationContext context, ElementAccessExpressionSyntax element, int id)
    {
        if (Evaluate(context, element.Expression.ToString()) is not ArrayMirror array)
            throw new ProtocolException("The selected value is not a managed array.");
        if (element.ArgumentList.Arguments.Count != array.Rank) throw new ProtocolException("Specify one index for each array dimension.");
        long offset = 0;
        for (int dimension = 0; dimension < array.Rank; dimension++)
        {
            var value = Evaluate(context, element.ArgumentList.Arguments[dimension].Expression.ToString());
            long index = value is PrimitiveValue primitive ? primitive.Value switch
            {
                sbyte number => number, byte number => number, short number => number, ushort number => number,
                int number => number, uint number => number, long number => number,
                ulong number when number <= long.MaxValue => (long)number, char character => character,
                ulong => throw new ProtocolException("The array element is outside its bounds."),
                _ => throw new ProtocolException("Array indices must evaluate to integers.")
            } : throw new ProtocolException("Array indices must evaluate to integers.");
            long lower = array.GetLowerBound(dimension);
            if (index < lower || index >= lower + array.GetLength(dimension)) throw new ProtocolException("The array element is outside its bounds.");
            offset = checked(offset * array.GetLength(dimension) + index - lower);
        }
        return new ManagedArrayBreakpoint(id, array, checked((int)offset));
    }
}
