using Isoplane.Build;
using Mono.Debugger.Soft;

namespace Isoplane.Debugging;

internal sealed record ActiveStatementRemapping(StackFrame Frame, int Version, int Offset)
{
    internal static ActiveStatementRemapping? Capture(StackFrame frame)
    {
        if (!StatementFlow.CanMove(frame.Method) || !frame.CanRemap) return null;
        var body = StatementFlow.Capture(frame.GetMethodBody());
        return StatementFlow.ReachableTargets(body, body, frame.ILOffset).Contains(frame.ILOffset)
            ? new(frame, frame.MethodVersion, frame.ILOffset) : null;
    }

    internal int? Resolve(ModuleUpdate[] updates)
    {
        if (Frame.IsCurrentMethodVersion) return Offset;
        var module = updates.SingleOrDefault(update => update.Module == Frame.Method.DeclaringType.Assembly.ManifestModule.ModuleVersionId);
        var statement = module?.ActiveStatements.SingleOrDefault(statement => statement.MethodToken == Frame.Method.MetadataToken
            && statement.MethodVersion == Version && statement.IlOffset == Offset);
        if (statement is null) return null;
        var span = statement.NewSpan;
        var target = Frame.Method.Locations.SingleOrDefault(location => location.LineNumber == span.StartLine + 1
            && location.ColumnNumber == span.StartColumn + 1
            && location.EndLineNumber == span.EndLine + 1 && location.EndColumnNumber == span.EndColumn + 1);
        return target?.ILOffset ?? throw new InvalidOperationException("The compiler's active statement has no matching executable location in the updated method.");
    }
}
