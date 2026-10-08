using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Mono.Cecil;
using Mono.Debugger.Soft;
using Mono.Debugging.Soft;

namespace Isoplane.Debugging;

internal sealed class StatementTargets(SoftDebuggerSession session, int threadId, Func<int> allocate, SourcePaths sources)
{
    private readonly Dictionary<int, int> offsets = [];

    internal GotoTargetsResponse Find(GotoTargetsArguments arguments)
    {
        string path = arguments.Source?.Path ?? throw new ProtocolException("A source path is required.");
        if (!Path.IsPathFullyQualified(path) || arguments.Line <= 0)
            throw new ProtocolException("Statement targets require an absolute source path and a positive line.");
        var result = new GotoTargetsResponse { Targets = [] };
        if (!session.CanSetNextStatement) return result;
        var frame = session.VirtualMachine.GetThreads().Single(thread => thread.Id == threadId).GetFrames().FirstOrDefault();
        if (frame?.Method is not { } method || !new GuestCode(session.VirtualMachine).Contains(method)) return result;
        using var bytes = new MemoryStream(method.DeclaringType.Assembly.GetMetadataBlob());
        using var metadata = AssemblyDefinition.ReadAssembly(bytes);
        if (metadata.MainModule.LookupToken(method.MetadataToken) is not MethodDefinition definition) return result;
        if (StatementFlow.IsAsyncBody(definition) && !IsExecutingAsyncBody(frame)) return result;
        if (!StatementFlow.CanMove(definition)) return result;
        var body = StatementFlow.Capture(method.GetMethodBody());
        var empty = StatementFlow.ReachableTargets(body, body, frame.ILOffset);
        if (!empty.Contains(frame.ILOffset)) return result;
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        foreach (var location in method.Locations.Where(location => location.LineNumber == arguments.Line
            && string.Equals(sources.ToClient(location.SourceFile), path, comparison) && empty.Contains(location.ILOffset)))
        {
            int id = allocate();
            offsets.Add(id, location.ILOffset);
            result.Targets.Add(new GotoTarget { Id = id, Label = method.Name + ":" + location.LineNumber,
                Line = location.LineNumber, Column = Math.Max(1, location.ColumnNumber) });
        }
        return result;
    }

    private static bool IsExecutingAsyncBody(Mono.Debugger.Soft.StackFrame frame)
    {
        var field = frame.Method.DeclaringType.GetField("<>1__state");
        if (field is null) return false;
        var state = frame.GetThis() switch
        {
            ObjectMirror instance => instance.GetValue(field),
            StructMirror instance => instance[field.Name],
            _ => null
        };
        // Roslyn uses -1 while executing user code, nonnegative states while awaiting,
        // and -2 after completion; only the executing state has an IP we can redirect.
        return state is PrimitiveValue { Value: -1 };
    }

    internal Mono.Debugging.Client.TargetEventArgs Move(int targetId)
    {
        if (!offsets.TryGetValue(targetId, out int offset))
            throw new ProtocolException("The statement target is not part of the current pause.");
        return session.MoveExecution(offset);
    }
}
