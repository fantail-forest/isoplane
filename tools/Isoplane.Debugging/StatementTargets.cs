using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Mono.Cecil;
using Mono.Debugging.Soft;

namespace Isoplane.Debugging;

internal sealed class StatementTargets(SoftDebuggerSession session, int threadId, Func<int> allocate)
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
        var empty = StatementFlow.EmptyStackOffsets(definition);
        if (!empty.Contains(frame.ILOffset)) return result;
        var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
        foreach (var location in method.Locations.Where(location => location.LineNumber == arguments.Line
            && string.Equals(location.SourceFile, path, comparison) && empty.Contains(location.ILOffset)))
        {
            int id = allocate();
            offsets.Add(id, location.ILOffset);
            result.Targets.Add(new GotoTarget { Id = id, Label = method.Name + ":" + location.LineNumber,
                Line = location.LineNumber, Column = Math.Max(1, location.ColumnNumber) });
        }
        return result;
    }

    internal Mono.Debugging.Client.TargetEventArgs Move(int targetId)
    {
        if (!offsets.TryGetValue(targetId, out int offset))
            throw new ProtocolException("The statement target is not part of the current pause.");
        return Mono.Debugging.Client.ExecutionPosition.Move(session, offset);
    }
}
